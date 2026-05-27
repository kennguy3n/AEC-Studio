//! Concrete `KChatPublisher` implementation that ships cards over the
//! local IPC socket to a running KChat Desktop instance.
//!
//! `LocalIpcPublisher` wraps a [`LocalIpcTransport`] and adds:
//!
//! - serialisation of [`ArtifactCard`] payloads (via the transport's
//!   discriminated-union request envelope),
//! - a small per-publisher dedup cache so re-publishing the same
//!   `(thread_id, project_link, caption)` triple is a no-op rather
//!   than a duplicate thread post,
//! - a public `ingest_reviews` hook the bridge polls every ~5 s to
//!   pull review comments into the audit trail.
//!
//! The publisher is `Send + Sync`. State is wrapped in a small
//! `Mutex<...>` so multiple bridge threads can call `publish` without
//! tripping over the transport's connection lifecycle.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use crate::kchat::{
    ArtifactCard, KChatError, KChatPublisher, PublishResult, ReviewCard, ReviewComment,
};
use crate::kchat_transport::LocalIpcTransport;

/// Default fallback thread identifier used by [`LocalIpcPublisher::publish`].
/// Real production paths always carry a thread id resolved from the
/// project's `KChatConfig`, but unit tests that haven't wired one yet
/// fall back to this constant so they don't have to thread state.
pub const DEFAULT_THREAD_ID: &str = "kchat-default";

/// Concrete publisher that ships cards to a local KChat Desktop
/// instance over [`LocalIpcTransport`].
#[derive(Debug)]
pub struct LocalIpcPublisher {
    transport: LocalIpcTransport,
    state: Mutex<PublisherState>,
}

#[derive(Debug, Default)]
struct PublisherState {
    /// `(thread_id, project_link, caption)` triples we've already
    /// published. Used to absorb double-clicks on the publish modal.
    dedup: HashSet<(String, String, String)>,
    /// Last successful publish — surfaced via [`LocalIpcPublisher::last_publish`].
    last_result: Option<PublishResult>,
}

impl LocalIpcPublisher {
    /// Build a publisher bound to an already-configured transport.
    pub fn new(transport: LocalIpcTransport) -> Self {
        Self {
            transport,
            state: Mutex::new(PublisherState::default()),
        }
    }

    /// Build a publisher with a fresh transport pointing at
    /// `socket_path`. Convenience for the common case where the
    /// caller doesn't already have a transport in hand.
    pub fn with_socket(socket_path: impl Into<std::path::PathBuf>) -> Self {
        Self::new(LocalIpcTransport::new(socket_path))
    }

    /// Build a publisher with a fresh transport and a caller-supplied
    /// timeout. Used by tests that need a tighter / looser budget.
    pub fn with_socket_timeout(
        socket_path: impl Into<std::path::PathBuf>,
        io_timeout: Duration,
    ) -> Self {
        Self::new(LocalIpcTransport::with_timeout(socket_path, io_timeout))
    }

    /// Borrow the underlying transport. Used by the bridge to
    /// surface the heartbeat / health-check status without going
    /// through the publisher (which would generate an audit-able
    /// publish call we don't want).
    pub fn transport(&self) -> &LocalIpcTransport {
        &self.transport
    }

    /// Public ingest hook. Called by the bridge on a ~5 s tick to
    /// pull new review comments out of KChat Desktop. The
    /// publisher does not maintain a `since` cursor itself — that's
    /// the caller's job, because the canonical cursor lives in the
    /// project's audit log.
    pub fn ingest_reviews(
        &self,
        thread_id: impl Into<String>,
        since_iso: Option<String>,
    ) -> Result<(Vec<ReviewComment>, Vec<ReviewCard>), KChatError> {
        self.transport.ingest_reviews(thread_id, since_iso)
    }

    /// Borrow the most recent publish result (used by integration
    /// tests). `None` until the first successful publish.
    pub fn last_publish(&self) -> Option<PublishResult> {
        self.state
            .lock()
            .expect("publisher state not poisoned")
            .last_result
            .clone()
    }

    fn dedup_key(card: &ArtifactCard, thread_id: &str) -> (String, String, String) {
        (
            thread_id.to_string(),
            card.project_link.clone(),
            card.caption.clone(),
        )
    }
}

impl KChatPublisher for LocalIpcPublisher {
    fn publish(&self, card: ArtifactCard) -> Result<PublishResult, KChatError> {
        card.validate()?;
        // Resolve the thread id from the card metadata if present,
        // else fall back to the project's default. Metadata keeps the
        // protocol forward-compatible without changing the public
        // ArtifactCard shape.
        let thread_id = card
            .metadata
            .get("thread_id")
            .cloned()
            .unwrap_or_else(|| DEFAULT_THREAD_ID.to_string());

        // Dedup: a repeated publish of the same card to the same
        // thread is a no-op. We still return the *previous* publish
        // result so the caller's audit chain doesn't break.
        {
            let mut state = self.state.lock().expect("publisher state not poisoned");
            let key = Self::dedup_key(&card, &thread_id);
            if state.dedup.contains(&key) {
                if let Some(prior) = &state.last_result {
                    return Ok(prior.clone());
                }
                // No prior result cached — fall through and try the
                // real publish. The dedup set will sync after.
            }
            state.dedup.insert(key);
        }

        let result = self.transport.publish(card)?;
        let mut state = self.state.lock().expect("publisher state not poisoned");
        state.last_result = Some(result.clone());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kchat::KChatArtifact;

    fn card_with_thread(thread: Option<&str>) -> ArtifactCard {
        let mut metadata = std::collections::HashMap::new();
        if let Some(t) = thread {
            metadata.insert("thread_id".into(), t.into());
        }
        ArtifactCard {
            artifact: KChatArtifact::ConceptRender,
            caption: "Living — warm evening".into(),
            project_link: "aecstudio://project/p1/renders/r1".into(),
            thumbnail_blake3: Some("a".repeat(64)),
            metadata,
        }
    }

    #[test]
    fn publisher_propagates_validate_errors() {
        let p = LocalIpcPublisher::with_socket("/nonexistent");
        let mut bad = card_with_thread(None);
        bad.caption.clear();
        let err = p.publish(bad).unwrap_err();
        assert!(matches!(err, KChatError::InvalidCard(_)), "got {err:?}");
    }

    #[test]
    fn publisher_returns_transport_error_when_socket_absent() {
        let p = LocalIpcPublisher::with_socket_timeout(
            "/nonexistent/no-kchat.sock",
            Duration::from_millis(50),
        );
        let err = p.publish(card_with_thread(None)).unwrap_err();
        assert!(matches!(err, KChatError::Transport(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn publisher_round_trips_against_mock_server() {
        use std::io::{BufRead, Read, Write};
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut buf = String::new();
            // 1st: publish
            reader.read_line(&mut buf).unwrap();
            assert!(buf.contains("\"kind\":\"publish\""));
            stream
                .write_all(
                    br#"{"kind":"publish_ok","result":{"message_id":"m-1","thread_id":"t-x","published_at":"2026-05-27T00:00:00Z"}}
"#,
                )
                .unwrap();
            // Drain the rest so the connection closes cleanly.
            let mut sink = Vec::new();
            let _ = reader.into_inner().read_to_end(&mut sink);
        });

        let p = LocalIpcPublisher::with_socket_timeout(&sock, Duration::from_secs(2));
        let r = p.publish(card_with_thread(Some("t-x"))).unwrap();
        assert_eq!(r.message_id, "m-1");
        assert_eq!(r.thread_id, "t-x");
        drop(p);
        let _ = handle.join();
    }

    #[cfg(unix)]
    #[test]
    fn second_publish_with_same_card_returns_cached_result() {
        use std::io::{BufRead, Read, Write};
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            // Only one publish should hit the wire — the second is
            // absorbed by the publisher's dedup cache.
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut buf = String::new();
            reader.read_line(&mut buf).unwrap();
            stream
                .write_all(
                    br#"{"kind":"publish_ok","result":{"message_id":"m-once","thread_id":"t-y","published_at":"2026-05-27T00:00:00Z"}}
"#,
                )
                .unwrap();
            // Don't accept again — if the publisher tried to re-publish,
            // this drop would close the listener and the second publish
            // would fail with a transport error.
            drop(stream);
            let mut sink = Vec::new();
            let _ = reader.into_inner().read_to_end(&mut sink);
        });

        let p = LocalIpcPublisher::with_socket_timeout(&sock, Duration::from_secs(2));
        let r1 = p.publish(card_with_thread(Some("t-y"))).unwrap();
        let r2 = p.publish(card_with_thread(Some("t-y"))).unwrap();
        assert_eq!(r1, r2);
        drop(p);
        let _ = handle.join();
    }

    #[cfg(unix)]
    #[test]
    fn ingest_reviews_returns_comments_and_cards() {
        use std::io::{BufRead, Read, Write};
        use std::os::unix::net::UnixListener;
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut buf = String::new();
            reader.read_line(&mut buf).unwrap();
            assert!(buf.contains("ingest_reviews"));
            let resp = serde_json::json!({
                "kind": "ingest_ok",
                "comments": [
                    {
                        "thread_id": "t-z",
                        "commenter": "ken@uney.com",
                        "text": "Looks good",
                        "timestamp": "2026-05-27T00:00:00Z",
                    }
                ],
                "cards": [],
            });
            stream.write_all(format!("{resp}\n").as_bytes()).unwrap();
            let mut sink = Vec::new();
            let _ = reader.into_inner().read_to_end(&mut sink);
        });

        let p = LocalIpcPublisher::with_socket_timeout(&sock, Duration::from_secs(2));
        let (comments, cards) = p.ingest_reviews("t-z", None).unwrap();
        assert_eq!(comments.len(), 1);
        assert!(cards.is_empty());
        assert_eq!(comments[0].text, "Looks good");
        drop(p);
        let _ = handle.join();
    }
}
