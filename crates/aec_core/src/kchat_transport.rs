//! Local IPC transport for KChat Desktop integration.
//!
//! KChat Desktop runs as a sibling Electron application on the same
//! machine and exposes a JSON-line protocol over a Unix domain socket
//! (macOS / Linux) or a named pipe (Windows). The transport is
//! deliberately tiny — three operations, each one message in / one
//! message out — because the bigger surface (publish framing,
//! review-comment ingest, dedup, audit chaining) is already handled
//! by [`crate::kchat`] and [`crate::kchat_sync`].
//!
//! The protocol is **request/response over a JSON line** (`\n`
//! terminator) — no streaming, no length prefix. Each request and
//! response carries a `kind` discriminator so the wire format is
//! forward-compatible with future additions.
//!
//! ## Wire format
//!
//! Request envelope:
//! ```json
//! {"kind": "publish", "card": <ArtifactCard JSON>}
//! ```
//! Response envelope (success):
//! ```json
//! {"kind": "publish_ok", "result": {"message_id": "...", "thread_id": "...", "published_at": "..."}}
//! ```
//! Response envelope (error):
//! ```json
//! {"kind": "error", "message": "..."}
//! ```
//!
//! ## Reconnect + heartbeat
//!
//! The transport keeps a single connection alive and reconnects with
//! exponential backoff (250 ms → 500 ms → 1 s → 2 s → 4 s, capped at
//! 4 s) when the socket drops. A heartbeat request is dispatched on
//! every [`LocalIpcTransport::health`] call — the bridge polls this
//! roughly every 5 s for the status indicator in the renderer.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::kchat::{ArtifactCard, KChatError, PublishResult, ReviewCard, ReviewComment};

/// Default per-request socket I/O timeout. Bounds how long a single
/// publish or ingest call can hang on a misbehaving KChat Desktop
/// instance.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Default heartbeat interval. The bridge polls health no faster
/// than once per [`DEFAULT_HEARTBEAT_INTERVAL`] so a flapping KChat
/// process doesn't peg the loopback socket.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Backoff schedule used by [`LocalIpcTransport::reconnect`]. The
/// final entry is the steady-state cap; once we hit it we stay there
/// until a connection finally succeeds.
const RECONNECT_BACKOFF: &[Duration] = &[
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// On-wire request envelope. Discriminated on `kind` so the wire
/// format is forward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Heartbeat / health-check. Server responds with [`IpcResponse::Pong`].
    Ping,
    /// Publish an artifact card.
    Publish { card: ArtifactCard },
    /// Ingest review comments newer than `since_iso`.
    IngestReviews {
        thread_id: String,
        since_iso: Option<String>,
    },
}

/// On-wire response envelope. The `error` variant is used for
/// transport / business errors; the typed `*_ok` variants carry
/// successful return values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcResponse {
    /// Heartbeat reply. Carries the server's reported version so the
    /// renderer can show "KChat Desktop 1.2.3" in the status panel.
    Pong { version: String },
    /// Successful publish.
    PublishOk { result: PublishResult },
    /// Successful review-comment ingest.
    IngestOk {
        comments: Vec<ReviewComment>,
        cards: Vec<ReviewCard>,
    },
    /// Business or transport error from the server side.
    Error { message: String },
}

/// Per-attempt failure kind. Used internally by
/// [`LocalIpcTransport::round_trip`] to decide whether a retry is
/// warranted: connection-level failures (`Transport`) walk the
/// backoff schedule, server-side error envelopes (`Remote`) bail
/// out immediately.
#[derive(Debug)]
enum AttemptError {
    Transport(KChatError),
    Remote(String),
}

/// One end of the local IPC link. Holds the path to the socket and
/// lazily opens a single long-lived connection.
#[derive(Debug)]
pub struct LocalIpcTransport {
    socket_path: PathBuf,
    io_timeout: Duration,
    state: Mutex<TransportState>,
}

#[derive(Debug, Default)]
struct TransportState {
    conn: Option<IpcConnection>,
    /// How many reconnect attempts have failed in a row since the
    /// last successful connect. Used to index into
    /// [`RECONNECT_BACKOFF`].
    consecutive_failures: usize,
    /// Last `Instant` we successfully sent a heartbeat. Used to
    /// throttle the status-indicator poll.
    last_heartbeat: Option<Instant>,
}

#[cfg(unix)]
mod platform {
    use std::io::{BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    pub struct PlatformConnection {
        pub stream: UnixStream,
        pub reader: BufReader<UnixStream>,
    }

    pub fn connect(path: &Path, timeout: Duration) -> std::io::Result<PlatformConnection> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(PlatformConnection { stream, reader })
    }

    impl PlatformConnection {
        pub fn write_line(&mut self, line: &[u8]) -> std::io::Result<()> {
            self.stream.write_all(line)?;
            self.stream.write_all(b"\n")?;
            self.stream.flush()
        }

        pub fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            buf.clear();
            // BufRead::read_line returns Ok(0) on EOF; the wrapper at
            // the call site converts that into a `Disconnected` error.
            use std::io::BufRead;
            self.reader.read_line(buf)
        }
    }

    // Silence the "unused import" lint that fires when we read but
    // don't write inside helper functions that don't use the
    // `Read` trait directly.
    #[allow(dead_code)]
    fn _silence_unused_read(_: &mut dyn Read) {}
}

#[cfg(windows)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::io::{BufReader, Write};
    use std::path::Path;
    use std::time::Duration;

    /// Windows named-pipe connection. Both reads and writes go
    /// through the same `File` handle, which the OS dispatches as a
    /// duplex pipe when opened with read+write access.
    pub struct PlatformConnection {
        pub stream: File,
        pub reader: BufReader<File>,
    }

    pub fn connect(path: &Path, _timeout: Duration) -> std::io::Result<PlatformConnection> {
        // Windows named pipes are opened via the regular file APIs.
        // Set FILE_FLAG_OVERLAPPED-equivalent behaviour is not
        // required for our small line-oriented protocol.
        let stream = OpenOptions::new().read(true).write(true).open(path)?;
        let reader_handle = stream.try_clone()?;
        let reader = BufReader::new(reader_handle);
        Ok(PlatformConnection { stream, reader })
    }

    impl PlatformConnection {
        pub fn write_line(&mut self, line: &[u8]) -> std::io::Result<()> {
            self.stream.write_all(line)?;
            self.stream.write_all(b"\n")?;
            self.stream.flush()
        }

        pub fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            use std::io::BufRead;
            buf.clear();
            self.reader.read_line(buf)
        }
    }
}

/// A live connection to a KChat Desktop instance. Hidden behind the
/// transport's `Mutex<TransportState>` so the public API stays
/// thread-safe even though the underlying stream is not.
#[derive(Debug)]
struct IpcConnection {
    inner: platform::PlatformConnection,
    /// `Instant` the connection was last successfully used. The
    /// transport drops connections older than 5 minutes to keep
    /// long-running desktop sessions from holding onto stale file
    /// descriptors.
    last_used: Instant,
}

impl std::fmt::Debug for platform::PlatformConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformConnection").finish_non_exhaustive()
    }
}

impl LocalIpcTransport {
    /// Build a transport bound to `socket_path`. No connection is
    /// opened yet — first I/O call lazily connects so a transport
    /// instance can sit dormant when KChat Desktop isn't running.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self::with_timeout(socket_path, DEFAULT_IO_TIMEOUT)
    }

    pub fn with_timeout(socket_path: impl Into<PathBuf>, io_timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            io_timeout,
            state: Mutex::new(TransportState::default()),
        }
    }

    /// Path the transport will connect to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Issue a single round-trip request, reconnecting on failure
    /// up to the backoff schedule. The transport surfaces the
    /// final error to the caller after the schedule is exhausted.
    ///
    /// `Error` envelope responses from the server are returned
    /// immediately without retry — they are well-formed protocol
    /// responses, not connection-level failures.
    pub fn round_trip(&self, request: &IpcRequest) -> Result<IpcResponse, KChatError> {
        let bytes = serde_json::to_vec(request)
            .map_err(|e| KChatError::Transport(format!("serialize: {e}")))?;

        // Try the existing connection first; if it fails, walk the
        // reconnect schedule. The state lock is held briefly during
        // each attempt so concurrent callers serialise but don't
        // deadlock on a slow remote.
        let mut last_err: Option<KChatError> = None;
        for _attempt in 0..=RECONNECT_BACKOFF.len() {
            match self.attempt_round_trip(&bytes) {
                Ok(resp) => return Ok(resp),
                Err(AttemptError::Remote(msg)) => {
                    // Well-formed Error envelope — no reconnect.
                    return Err(KChatError::Transport(msg));
                }
                Err(AttemptError::Transport(e)) => {
                    last_err = Some(e);
                    self.sleep_backoff();
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            KChatError::Transport("no connection attempts produced an error".into())
        }))
    }

    fn attempt_round_trip(&self, bytes: &[u8]) -> Result<IpcResponse, AttemptError> {
        let mut state = self.state.lock().expect("transport state not poisoned");
        if state.conn.is_none() {
            let conn = self
                .connect_locked(&mut state)
                .map_err(AttemptError::Transport)?;
            state.conn = Some(conn);
        }

        // Run the round-trip in a sub-scope so the &mut borrow on
        // `state.conn` is released before we touch other fields.
        enum Outcome {
            Ok(IpcResponse),
            Drop(KChatError),
        }
        let outcome: Outcome = {
            let conn = state.conn.as_mut().expect("connection just inserted above");
            if let Err(e) = conn.inner.write_line(bytes) {
                Outcome::Drop(KChatError::Transport(format!("write: {e}")))
            } else {
                let mut line = String::new();
                match conn.inner.read_line(&mut line) {
                    Ok(0) => {
                        Outcome::Drop(KChatError::Transport("peer closed the connection".into()))
                    }
                    Ok(_) => match serde_json::from_str::<IpcResponse>(line.trim_end()) {
                        Ok(resp) => {
                            conn.last_used = Instant::now();
                            Outcome::Ok(resp)
                        }
                        Err(e) => Outcome::Drop(KChatError::Transport(format!("decode: {e}"))),
                    },
                    Err(e)
                        if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                    {
                        Outcome::Drop(KChatError::Transport("io timeout".into()))
                    }
                    Err(e) => Outcome::Drop(KChatError::Transport(format!("read: {e}"))),
                }
            }
        };
        match outcome {
            Outcome::Ok(IpcResponse::Error { message }) => {
                // Server returned a well-formed error envelope. The
                // connection is healthy — don't tear it down, don't
                // count toward the retry budget.
                state.consecutive_failures = 0;
                Err(AttemptError::Remote(message))
            }
            Outcome::Ok(resp) => {
                state.consecutive_failures = 0;
                Ok(resp)
            }
            Outcome::Drop(err) => {
                state.conn = None;
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                Err(AttemptError::Transport(err))
            }
        }
    }

    fn connect_locked(&self, state: &mut TransportState) -> Result<IpcConnection, KChatError> {
        match platform::connect(&self.socket_path, self.io_timeout) {
            Ok(inner) => {
                state.consecutive_failures = 0;
                Ok(IpcConnection {
                    inner,
                    last_used: Instant::now(),
                })
            }
            Err(e) => {
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                Err(KChatError::Transport(format!(
                    "connect {:?}: {e}",
                    self.socket_path.display()
                )))
            }
        }
    }

    fn sleep_backoff(&self) {
        let state = self.state.lock().expect("transport state not poisoned");
        let idx = state
            .consecutive_failures
            .saturating_sub(1)
            .min(RECONNECT_BACKOFF.len() - 1);
        let delay = RECONNECT_BACKOFF[idx];
        drop(state);
        std::thread::sleep(delay);
    }

    /// Health-check / heartbeat. Returns the server-reported version
    /// on success and `None` if the transport hasn't been able to
    /// reach KChat Desktop.
    pub fn health(&self) -> Option<String> {
        match self.round_trip(&IpcRequest::Ping) {
            Ok(IpcResponse::Pong { version }) => {
                let mut state = self.state.lock().expect("transport state not poisoned");
                state.last_heartbeat = Some(Instant::now());
                Some(version)
            }
            _ => None,
        }
    }

    /// Send an artifact card. Returns the publisher's [`PublishResult`].
    pub fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        match self.round_trip(&IpcRequest::Publish { card })? {
            IpcResponse::PublishOk { result } => Ok(result),
            other => Err(KChatError::Transport(format!(
                "unexpected publish response: {other:?}"
            ))),
        }
    }

    /// Poll for new review comments on `thread_id`. `since_iso` is
    /// the last seen timestamp; the server returns only newer
    /// entries. Both ad-hoc comments and explicit `ReviewCard`s
    /// (which carry an approval state) are returned.
    pub fn ingest_reviews(
        &self,
        thread_id: impl Into<String>,
        since_iso: Option<String>,
    ) -> Result<(Vec<ReviewComment>, Vec<ReviewCard>), KChatError> {
        match self.round_trip(&IpcRequest::IngestReviews {
            thread_id: thread_id.into(),
            since_iso,
        })? {
            IpcResponse::IngestOk { comments, cards } => Ok((comments, cards)),
            other => Err(KChatError::Transport(format!(
                "unexpected ingest response: {other:?}"
            ))),
        }
    }

    /// How many seconds ago the last successful heartbeat landed.
    /// `None` means "never" — the indicator UI surfaces that as
    /// "disconnected" rather than "reconnecting".
    pub fn seconds_since_heartbeat(&self) -> Option<u64> {
        let state = self.state.lock().expect("transport state not poisoned");
        state.last_heartbeat.map(|t| t.elapsed().as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kchat::KChatArtifact;
    use std::io::Write;

    #[test]
    fn ipc_request_serialises_with_kind_discriminator() {
        let req = IpcRequest::Ping;
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"kind\":\"ping\""), "got {s}");
    }

    #[test]
    fn ipc_response_pong_roundtrips() {
        let r = IpcResponse::Pong {
            version: "1.2.3".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: IpcResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn ipc_request_publish_carries_card() {
        let card = ArtifactCard {
            artifact: KChatArtifact::ConceptRender,
            caption: "test".into(),
            project_link: "aecstudio://project/abc".into(),
            thumbnail_blake3: None,
            metadata: std::collections::HashMap::new(),
        };
        let req = IpcRequest::Publish { card: card.clone() };
        let s = serde_json::to_string(&req).unwrap();
        let back: IpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
        if let IpcRequest::Publish { card: c } = back {
            assert_eq!(c.caption, card.caption);
        } else {
            panic!("expected Publish variant");
        }
    }

    #[test]
    fn transport_initialises_without_connecting() {
        let t = LocalIpcTransport::new("/nonexistent/path/will-not-be-touched");
        assert!(t.seconds_since_heartbeat().is_none());
    }

    #[test]
    fn transport_returns_transport_error_when_socket_absent() {
        let t = LocalIpcTransport::with_timeout(
            "/nonexistent/path/should-fail",
            Duration::from_millis(50),
        );
        let res = t.round_trip(&IpcRequest::Ping);
        assert!(matches!(res, Err(KChatError::Transport(_))), "got {res:?}");
    }

    #[cfg(unix)]
    #[test]
    fn transport_round_trips_against_mock_socket_server() {
        use std::io::Read;
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Spawn the mock server on a thread. Speaks one ping, then
        // one publish, then closes.
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read one line at a time and respond.
            let mut buf = String::new();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            use std::io::BufRead;
            // 1st: ping
            reader.read_line(&mut buf).unwrap();
            stream
                .write_all(b"{\"kind\":\"pong\",\"version\":\"42.0.0\"}\n")
                .unwrap();
            buf.clear();
            // 2nd: publish
            reader.read_line(&mut buf).unwrap();
            assert!(buf.contains("\"kind\":\"publish\""), "got {buf}");
            let pub_resp = serde_json::json!({
                "kind": "publish_ok",
                "result": {
                    "message_id": "msg-1",
                    "thread_id": "thread-test",
                    "published_at": "2026-05-27T00:00:00Z",
                },
            });
            stream
                .write_all(format!("{pub_resp}\n").as_bytes())
                .unwrap();
            // Drain the closing read; the test doesn't care.
            let mut trailing = Vec::new();
            let _ = reader.into_inner().read_to_end(&mut trailing);
            // Silence unused warning - we only care about side-effect.
            let _ = trailing.len();
        });

        let t = LocalIpcTransport::with_timeout(&sock_path, Duration::from_secs(2));
        // Ping returns the server-reported version.
        let v = t.health();
        assert_eq!(v.as_deref(), Some("42.0.0"));
        // Publish round-trips.
        let card = ArtifactCard {
            artifact: KChatArtifact::ConceptRender,
            caption: "Hello".into(),
            project_link: "aecstudio://project/x/r/y".into(),
            thumbnail_blake3: None,
            metadata: std::collections::HashMap::new(),
        };
        let result = t.publish(card).unwrap();
        assert_eq!(result.thread_id, "thread-test");

        // Shut the mock server down so the test process can exit
        // cleanly even if the connection is still cached on the
        // transport side.
        drop(t);
        let _ = handle.join();
    }

    #[cfg(unix)]
    #[test]
    fn transport_surfaces_error_envelope_as_kchat_error() {
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = String::new();
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            reader.read_line(&mut buf).unwrap();
            stream
                .write_all(b"{\"kind\":\"error\",\"message\":\"thread not found\"}\n")
                .unwrap();
        });
        let t = LocalIpcTransport::with_timeout(&sock, Duration::from_secs(2));
        let err = t.round_trip(&IpcRequest::Ping).unwrap_err();
        assert!(
            matches!(err, KChatError::Transport(m) if m.contains("thread not found")),
            "wrong err"
        );
        drop(t);
        let _ = handle.join();
    }
}
