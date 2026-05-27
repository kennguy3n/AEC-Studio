//! Auto-detection of a locally running KChat Desktop instance.
//!
//! KChat Desktop, when it starts, creates a well-known socket / pipe
//! that AEC Studio can connect to without any user configuration.
//! [`KChatDiscovery::probe`] checks the platform-specific path and,
//! if the socket is reachable, returns a [`KChatInstanceInfo`] with
//! the version string the instance reports.
//!
//! The probe is cheap (one `connect` + one ping round-trip on a 200 ms
//! timeout) and is safe to call on a UI tick — the renderer's status
//! indicator polls it every 5 s.
//!
//! ## Platform paths
//!
//! - **macOS**: `$HOME/Library/Application Support/KChat/ipc.sock`
//! - **Linux**: `$XDG_RUNTIME_DIR/kchat/ipc.sock`, falling back to
//!   `/run/user/<uid>/kchat/ipc.sock` and then `/tmp/kchat-<uid>.sock`
//! - **Windows**: `\\.\pipe\kchat-ipc`
//!
//! An `AEC_KCHAT_SOCKET_PATH` environment variable overrides every
//! platform default — used by integration tests and by packaged
//! builds that want to point at a non-standard socket.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::kchat_transport::LocalIpcTransport;

/// Environment override for the discovered socket path. Set in
/// integration tests via `std::env::set_var` so the probe targets
/// the test fixture's `UnixListener` instead of the user's real
/// KChat instance.
pub const KCHAT_SOCKET_PATH_ENV: &str = "AEC_KCHAT_SOCKET_PATH";

/// Snapshot of a discovered KChat Desktop instance. Returned by
/// [`KChatDiscovery::probe`] when a healthy responder is found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KChatInstanceInfo {
    /// Absolute path to the socket / pipe.
    pub socket_path: PathBuf,
    /// Version string reported by the instance's `pong` reply.
    pub version: String,
    /// Health status — one of `connected`, `disconnected`, `reconnecting`.
    pub health: String,
}

/// Discovery entry point. All methods are associated functions —
/// there is no per-instance state to keep, because the underlying
/// transport handles connection lifetime.
pub struct KChatDiscovery;

impl KChatDiscovery {
    /// Probe the well-known KChat Desktop socket path. Returns `None`
    /// when the socket is absent, unresponsive, or returns a
    /// non-pong reply.
    pub fn probe() -> Option<KChatInstanceInfo> {
        Self::probe_with_timeout(Duration::from_millis(200))
    }

    /// Probe with a caller-supplied timeout. Useful for tests that
    /// want a longer budget than the default 200 ms.
    pub fn probe_with_timeout(io_timeout: Duration) -> Option<KChatInstanceInfo> {
        let path = Self::resolved_socket_path()?;
        if !Self::socket_exists(&path) {
            return None;
        }
        let transport = LocalIpcTransport::with_timeout(&path, io_timeout);
        let version = transport.health()?;
        Some(KChatInstanceInfo {
            socket_path: path,
            version,
            health: "connected".into(),
        })
    }

    /// The path the discovery layer would probe on this platform.
    /// Returns `None` if the platform doesn't expose a meaningful
    /// default (e.g. an unrecognised OS).
    pub fn resolved_socket_path() -> Option<PathBuf> {
        if let Ok(p) = std::env::var(KCHAT_SOCKET_PATH_ENV) {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        Self::platform_default_path()
    }

    /// Default socket path *ignoring* the environment override. Used
    /// by tests that want to assert against the platform fallback
    /// even when the env var has been set by an earlier test in the
    /// same process.
    pub fn platform_default_path() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let home = std::env::var("HOME").ok()?;
            Some(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("KChat")
                    .join("ipc.sock"),
            )
        }
        #[cfg(target_os = "linux")]
        {
            // Prefer $XDG_RUNTIME_DIR, then /run/user/<uid>, then /tmp.
            if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
                if !dir.is_empty() {
                    return Some(PathBuf::from(dir).join("kchat").join("ipc.sock"));
                }
            }
            // Best-effort UID lookup. We avoid pulling in `nix` /
            // `libc` to keep this crate dependency-light; the
            // env-var fallback below covers the path where UID isn't
            // observable.
            let uid = unsafe_uid_or(1000);
            let runtime = PathBuf::from(format!("/run/user/{uid}/kchat/ipc.sock"));
            if runtime.parent().is_some_and(Path::exists) {
                return Some(runtime);
            }
            Some(PathBuf::from(format!("/tmp/kchat-{uid}.sock")))
        }
        #[cfg(target_os = "windows")]
        {
            Some(PathBuf::from(r"\\.\pipe\kchat-ipc"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }

    /// Whether the discovery socket file is currently present. For
    /// Unix sockets this is a `path.exists()`; for Windows named
    /// pipes there's no filesystem entry, so we return `true` and
    /// let the actual connect attempt decide.
    pub fn socket_exists(path: &Path) -> bool {
        if cfg!(windows) {
            true
        } else {
            path.exists()
        }
    }

    /// Build a [`LocalIpcTransport`] targeted at the discovered
    /// socket (or the env override). Returns `None` if no path could
    /// be resolved at all.
    pub fn transport(io_timeout: Duration) -> Option<LocalIpcTransport> {
        Self::resolved_socket_path().map(|p| LocalIpcTransport::with_timeout(p, io_timeout))
    }
}

/// Cheap UID lookup that avoids the `libc` / `nix` dependency. Reads
/// `/proc/self/loginuid` first (which is what systemd's session
/// manager writes), and falls back to `USER`-mapped defaults. Used
/// only on Linux. Wrapped in a function so the `unsafe`-style name
/// stays at the call site for grepability — there is no actual
/// `unsafe` block in here.
#[cfg(target_os = "linux")]
fn unsafe_uid_or(default: u32) -> u32 {
    std::fs::read_to_string("/proc/self/loginuid")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&u| u != u32::MAX)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-global lock for env-var-mutating tests. `std::env`
    /// mutations leak across the cargo test thread pool so two
    /// concurrent tests would race when both touch
    /// `AEC_KCHAT_SOCKET_PATH`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn platform_default_path_is_non_empty_on_supported_oses() {
        let p = KChatDiscovery::platform_default_path();
        if cfg!(target_os = "macos") || cfg!(target_os = "linux") || cfg!(target_os = "windows") {
            assert!(p.is_some());
            assert!(!p.unwrap().as_os_str().is_empty());
        }
    }

    #[test]
    fn env_override_takes_precedence_over_platform_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var(KCHAT_SOCKET_PATH_ENV).ok();
        std::env::set_var(KCHAT_SOCKET_PATH_ENV, "/tmp/some-override.sock");
        let p = KChatDiscovery::resolved_socket_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/some-override.sock"));
        // Restore.
        match saved {
            Some(v) => std::env::set_var(KCHAT_SOCKET_PATH_ENV, v),
            None => std::env::remove_var(KCHAT_SOCKET_PATH_ENV),
        }
    }

    #[test]
    fn probe_returns_none_when_socket_absent() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = std::env::var(KCHAT_SOCKET_PATH_ENV).ok();
        std::env::set_var(
            KCHAT_SOCKET_PATH_ENV,
            "/var/empty/definitely-not-a-real-kchat.sock",
        );
        let info = KChatDiscovery::probe_with_timeout(Duration::from_millis(50));
        assert!(info.is_none());
        match saved {
            Some(v) => std::env::set_var(KCHAT_SOCKET_PATH_ENV, v),
            None => std::env::remove_var(KCHAT_SOCKET_PATH_ENV),
        }
    }

    #[cfg(unix)]
    #[test]
    fn probe_returns_info_when_mock_socket_responds() {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixListener;
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("kchat.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut buf = String::new();
            reader.read_line(&mut buf).unwrap();
            stream
                .write_all(b"{\"kind\":\"pong\",\"version\":\"7.8.9\"}\n")
                .unwrap();
        });

        let saved = std::env::var(KCHAT_SOCKET_PATH_ENV).ok();
        std::env::set_var(KCHAT_SOCKET_PATH_ENV, &sock);
        let info = KChatDiscovery::probe_with_timeout(Duration::from_secs(1));
        match saved {
            Some(v) => std::env::set_var(KCHAT_SOCKET_PATH_ENV, v),
            None => std::env::remove_var(KCHAT_SOCKET_PATH_ENV),
        }
        let info = info.expect("probe should succeed");
        assert_eq!(info.version, "7.8.9");
        assert_eq!(info.health, "connected");
        let _ = handle.join();
    }
}
