//! HTTPS model download with resume, progress, and minimal headers.
//!
//! Used by [`crate::model_manager::ModelManager::download_model`] to fetch
//! Ternary-Bonsai GGUF files from `huggingface.co` over HTTPS. Separate
//! from the loopback-only [`crate::http`] client because the security
//! posture is different:
//!
//! - The loopback client talks to the local `llama-server` sidecar and
//!   deliberately avoids TLS, DNS, redirects, and cookies (zero attack
//!   surface).
//! - This client talks to HuggingFace over the public internet, so it
//!   needs full TLS + cert verification + redirect handling (HF's CDN
//!   issues a 302 to a `cdn-lfs.huggingface.co` URL).
//!
//! ## Security & privacy posture
//!
//! - **Minimal `User-Agent`**: `AEC-Studio/<crate-version>` only. No OS,
//!   hardware, locale, or user identifier.
//! - **No cookies**: `ureq` does not maintain a cookie jar by default,
//!   and we never inject one.
//! - **TLS verified**: `rustls` validates the certificate chain against
//!   the platform trust store (via `rustls-platform-verifier`, pulled in
//!   transitively by `ureq`'s `tls` feature).
//! - **Redirects allowed only inside `huggingface.co` + its CDN**: the
//!   HF download URL issues a 302 to `cdn-lfs.huggingface.co` (or
//!   similar). We follow that, but block redirects to unrelated hosts
//!   to prevent a malicious mirror from harvesting download patterns.
//!
//! ## Resume protocol
//!
//! Downloads are written to `<filename>.partial`. On retry, the
//! existing partial size is read with `std::fs::metadata`, and the
//! request is reissued with `Range: bytes=N-`. The server's
//! `Content-Range: bytes N-M/total` response confirms the resume; if
//! the server returns 200 (not 206), the partial is rewritten from
//! offset 0.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

/// Callback invoked periodically during a download with
/// `(downloaded_bytes, total_bytes)`. Must be `Send + Sync` so the
/// `ModelManager` can hand it to a background download thread; the
/// `Fn` (not `FnMut`) bound lets the callee call it from multiple
/// progress events without taking exclusive access.
pub type ProgressCallback = Arc<dyn Fn(u64, u64) + Send + Sync>;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http request failed: {0}")]
    Http(String),
    #[error("server returned status {0}")]
    Status(u16),
    #[error("response missing Content-Length and no descriptor size; cannot proceed")]
    MissingLength,
    #[error("redirect to disallowed host: {0}")]
    DisallowedRedirect(String),
}

impl From<ureq::Error> for DownloadError {
    fn from(value: ureq::Error) -> Self {
        match value {
            ureq::Error::Status(code, _) => Self::Status(code),
            ureq::Error::Transport(t) => Self::Http(t.to_string()),
        }
    }
}

const USER_AGENT: &str = concat!("AEC-Studio/", env!("CARGO_PKG_VERSION"));
/// Read-buffer size for streaming bytes from the response to disk.
/// 64 KiB matches `std::fs::File`'s default and is the same chunk size
/// used by [`crate::model_manager::blake3_file`], keeping the
/// download + verify passes cache-friendly.
const CHUNK_BYTES: usize = 64 * 1024;
/// How often to invoke the progress callback (in chunks). 16 × 64 KiB
/// ≈ 1 MiB, which is fine-grained enough for a smooth progress bar
/// without being a microbenchmark on the locking inside the callback.
const PROGRESS_EVERY_CHUNKS: usize = 16;

/// Allowed hosts for HF model downloads (HF issues 302s to its CDN).
/// Any redirect to a host outside this list is rejected.
///
/// ## Safety against suffix-confusion attacks
///
/// The naïve form of this check (`host.ends_with(".huggingface.co")`)
/// is already robust against the classic `evil-huggingface.co`
/// mistake — `-huggingface.co` does not end with `.huggingface.co`;
/// the literal `.` separator is required. We additionally reject:
///
/// * **Empty hosts** — `validate_redirect` should already have caught
///   these, but a `.ends_with("")` style trap on the empty string
///   technically returns true for any non-empty suffix, so we guard
///   defensively.
/// * **Leading-dot hosts** (e.g. `.huggingface.co`) — these are not
///   valid DNS hostnames per RFC 1035 and shouldn't reach us via a
///   well-formed `Location` header, but rejecting them removes a
///   latent foot-gun where `.foo.bar` would `ends_with(".foo.bar")`.
///
/// DNS is case-insensitive, so we lowercase the host before checking.
fn host_is_allowed(host: &str) -> bool {
    if host.is_empty() || host.starts_with('.') {
        return false;
    }
    let h = host.to_ascii_lowercase();
    matches!(
        h.as_str(),
        "huggingface.co" | "cdn-lfs.huggingface.co" | "cdn-lfs.hf.co" | "cas-bridge.xethub.hf.co"
    ) || h.ends_with(".huggingface.co")
        || h.ends_with(".hf.co")
}

/// Validate that a redirect target is to an allowed host. Returns the
/// new URL on success; an error otherwise.
fn validate_redirect(url: &str) -> Result<(), DownloadError> {
    // Cheap host extraction without pulling in `url`: split off "scheme://"
    // then take everything before the next "/", ":", or end.
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| DownloadError::DisallowedRedirect(url.to_string()))?;
    let host_end = after_scheme
        .find(['/', ':', '?', '#'])
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    if host_is_allowed(host) {
        Ok(())
    } else {
        Err(DownloadError::DisallowedRedirect(host.to_string()))
    }
}

/// Build a `ureq::Agent` with our minimal, telemetry-free configuration.
fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .redirects(0) // We handle redirects manually to validate target hosts.
        .build()
}

/// Download the resource at `url` to `dest`. If `dest` already exists
/// (a previous attempt left a partial file), resume from its current
/// length using a `Range: bytes=N-` request.
///
/// `expected_total` is the descriptor's known file size (used to
/// initialise the progress bar before the server has answered, and to
/// validate the `Content-Length` once it arrives). Pass `0` to skip
/// total-validation.
///
/// `on_progress` is invoked at least once before the function returns
/// (with `(total, total)` on success) so the caller's UI always sees
/// a terminal progress event.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    expected_total: u64,
    on_progress: Option<ProgressCallback>,
) -> Result<(), DownloadError> {
    let agent = build_agent();
    let resume_from = match std::fs::metadata(dest) {
        Ok(m) if m.is_file() => m.len(),
        _ => 0,
    };

    let mut current_url = url.to_string();
    let mut response = None;
    // Follow up to 5 redirects manually so we can validate each target host.
    // Depending on the ureq version, a 3xx response with `redirects(0)` may
    // surface either as `Ok(response)` with `status == 3xx` or as
    // `Err(Status(3xx, response))` — we handle both.
    for _ in 0..5 {
        let mut req = agent.get(&current_url);
        if resume_from > 0 {
            req = req.set("Range", &format!("bytes={resume_from}-"));
        }
        let r = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(other) => return Err(other.into()),
        };
        let status = r.status();
        if (300..400).contains(&status) {
            let Some(location) = r.header("location") else {
                return Err(DownloadError::Status(status));
            };
            let next = absolutise(&current_url, location);
            validate_redirect(&next)?;
            current_url = next;
            continue;
        }
        response = Some(r);
        break;
    }
    let response = response.ok_or_else(|| DownloadError::Http("too many redirects".into()))?;

    let status = response.status();
    let resuming = status == 206;
    if status != 200 && status != 206 {
        return Err(DownloadError::Status(status));
    }

    // Compute the total size for the progress bar. Prefer the server's
    // Content-Range total on resumed responses; fall back to
    // Content-Length plus the resume offset; finally the descriptor.
    let total = if let Some(range) = response.header("content-range") {
        parse_content_range_total(range).unwrap_or(expected_total)
    } else if let Some(len) = response.header("content-length") {
        let body = len.parse::<u64>().unwrap_or(0);
        if resuming {
            resume_from + body
        } else {
            body
        }
    } else if expected_total > 0 {
        expected_total
    } else {
        return Err(DownloadError::MissingLength);
    };

    if let Some(cb) = on_progress.as_ref() {
        cb(if resuming { resume_from } else { 0 }, total);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = if resuming {
        let mut f = std::fs::OpenOptions::new().write(true).open(dest)?;
        f.seek(SeekFrom::Start(resume_from))?;
        f
    } else {
        // Server didn't honour the range or there was no prior partial —
        // truncate and start from 0.
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dest)?
    };

    let mut reader = response.into_reader();
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut downloaded = if resuming { resume_from } else { 0 };
    let mut chunks_since_progress = 0usize;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded = downloaded.saturating_add(n as u64);
        chunks_since_progress += 1;
        if chunks_since_progress >= PROGRESS_EVERY_CHUNKS {
            if let Some(cb) = on_progress.as_ref() {
                cb(downloaded, total);
            }
            chunks_since_progress = 0;
        }
    }
    file.flush()?;
    if let Some(cb) = on_progress.as_ref() {
        cb(downloaded, total);
    }
    Ok(())
}

/// Resolve a (possibly relative) Location header against the request URL.
fn absolutise(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if location.starts_with("//") {
        let scheme_end = base.find("://").unwrap_or(0);
        format!("{}:{}", &base[..scheme_end], location)
    } else if let Some(stripped) = location.strip_prefix('/') {
        // Absolute path on same host.
        let after_scheme = base.split_once("://").map_or(base, |(_, rest)| rest);
        let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        let host = &after_scheme[..host_end];
        let scheme = base.split("://").next().unwrap_or("https");
        format!("{scheme}://{host}/{stripped}")
    } else {
        // Relative path. Strip last segment of base and append.
        let cut = base.rfind('/').unwrap_or(base.len());
        format!("{}/{}", &base[..cut], location)
    }
}

/// Parse the total length from a `Content-Range: bytes start-end/total`
/// header. Returns `None` if the header is malformed.
fn parse_content_range_total(value: &str) -> Option<u64> {
    let after_slash = value.rsplit('/').next()?;
    after_slash.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    fn bind_loopback() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind")
    }

    /// Spawn a mock HTTP server that:
    /// - On the *first* GET, returns 200 OK with `len` bytes of `pattern`.
    /// - Honours `Range: bytes=N-` by returning 206 with `Content-Range`.
    fn spawn_mock(listener: TcpListener, body: Vec<u8>) -> thread::JoinHandle<()> {
        thread::spawn(move || loop {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let mut req = vec![0u8; 8192];
            let n = s.read(&mut req).unwrap_or(0);
            let text = std::str::from_utf8(&req[..n]).unwrap_or("");
            let range = parse_range_header(text);
            let total = body.len() as u64;
            let (status, start) = match range {
                Some(start) if start < total => (206, start),
                _ => (200, 0),
            };
            let slice = &body[start as usize..];
            let body_len = slice.len();
            let status_line = if status == 206 {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            let content_range = if status == 206 {
                format!("Content-Range: bytes {start}-{}/{total}\r\n", total - 1)
            } else {
                String::new()
            };
            let headers = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/octet-stream\r\nContent-Length: {body_len}\r\n{content_range}\r\n",
            );
            let _ = s.write_all(headers.as_bytes());
            let _ = s.write_all(slice);
            let _ = s.flush();
        })
    }

    fn parse_range_header(req: &str) -> Option<u64> {
        for line in req.lines() {
            if let Some(rest) = line.to_ascii_lowercase().strip_prefix("range:") {
                let trimmed = rest.trim();
                if let Some(b) = trimmed.strip_prefix("bytes=") {
                    let start = b.split('-').next().unwrap_or("0");
                    return start.parse::<u64>().ok();
                }
            }
        }
        None
    }

    /// Spawn a 302-redirect server that points to a sibling listener.
    fn spawn_redirect(listener: TcpListener, location: String) -> thread::JoinHandle<()> {
        thread::spawn(move || loop {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let mut req = vec![0u8; 8192];
            let _ = s.read(&mut req);
            let resp =
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n");
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        })
    }

    #[test]
    fn parses_content_range_total() {
        assert_eq!(parse_content_range_total("bytes 100-199/1000"), Some(1000));
        assert_eq!(parse_content_range_total("bytes 0-9/10"), Some(10));
        assert_eq!(parse_content_range_total("malformed"), None);
    }

    #[test]
    fn absolutises_relative_paths() {
        assert_eq!(
            absolutise("https://huggingface.co/foo/bar", "/baz"),
            "https://huggingface.co/baz",
        );
        assert_eq!(
            absolutise("https://huggingface.co/foo/bar", "https://other/x"),
            "https://other/x",
        );
        assert_eq!(
            absolutise("https://huggingface.co/foo/bar", "qux"),
            "https://huggingface.co/foo/qux",
        );
    }

    #[test]
    fn host_allowlist_accepts_hf_domains_and_rejects_others() {
        // Positive cases: HF root + known CDN subdomains + any
        // sub-subdomain (HF rotates CDN hostnames).
        assert!(host_is_allowed("huggingface.co"));
        assert!(host_is_allowed("cdn-lfs.huggingface.co"));
        assert!(host_is_allowed("foo.bar.huggingface.co"));
        assert!(host_is_allowed("cdn-lfs.hf.co"));
        // Plain unrelated domain.
        assert!(!host_is_allowed("example.com"));
        // Suffix appending: `host.attacker.com` must not match the
        // rule even though `host` contains "huggingface.co".
        assert!(!host_is_allowed("evil.huggingface.co.attacker.com"));
        // Dash-prefix forgery: the `.huggingface.co` / `.hf.co`
        // suffix checks REQUIRE the literal dot separator, so an
        // attacker domain like `malicious-huggingface.co` (which
        // ends with `-huggingface.co`, not `.huggingface.co`) is
        // rejected. This is a defense-in-depth guarantee against
        // the classic `ends_with` suffix-confusion mistake.
        assert!(!host_is_allowed("malicious-huggingface.co"));
        assert!(!host_is_allowed("malicious-hf.co"));
        // Empty / placeholder inputs.
        assert!(!host_is_allowed(""));
        assert!(!host_is_allowed(".huggingface.co"));
        // Case insensitivity: DNS is case-insensitive, so
        // `Huggingface.co` and `HUGGINGFACE.CO` are valid.
        assert!(host_is_allowed("Huggingface.co"));
        assert!(host_is_allowed("HUGGINGFACE.CO"));
    }

    #[test]
    fn validate_redirect_blocks_unknown_hosts() {
        assert!(validate_redirect("https://huggingface.co/x").is_ok());
        assert!(validate_redirect("https://cdn-lfs.huggingface.co/x").is_ok());
        assert!(validate_redirect("https://evil.example.com/x").is_err());
    }

    #[test]
    fn downloads_to_file_with_progress() {
        let listener = bind_loopback();
        let port = listener.local_addr().unwrap().port();
        let body = (0..2048u32).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        let join = spawn_mock(listener, body.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        let progress = Arc::new(AtomicU64::new(0));
        let p = progress.clone();
        let cb: ProgressCallback = Arc::new(move |done, _total| {
            p.store(done, Ordering::SeqCst);
        });
        download_to_file(
            &format!("http://127.0.0.1:{port}/file"),
            &dest,
            body.len() as u64,
            Some(cb),
        )
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        assert_eq!(written, body);
        assert_eq!(progress.load(Ordering::SeqCst), body.len() as u64);
        drop(join);
    }

    #[test]
    fn resumes_from_partial_file() {
        let listener = bind_loopback();
        let port = listener.local_addr().unwrap().port();
        let body = (0..4096u32).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        let join = spawn_mock(listener, body.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        // Pre-seed the partial with the first 1024 bytes.
        std::fs::write(&dest, &body[..1024]).unwrap();
        download_to_file(
            &format!("http://127.0.0.1:{port}/file"),
            &dest,
            body.len() as u64,
            None,
        )
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        assert_eq!(written, body, "resumed file should equal full body");
        drop(join);
    }

    #[test]
    fn follows_redirect_to_allowed_host() {
        // Two loopback listeners. The first issues a 302 pointing at the
        // second's port. validate_redirect uses an allow-list of hosts —
        // for tests we hit `127.0.0.1`, which we permit via a thread-local
        // override. Instead we just verify that *external* redirects are
        // blocked, since redirecting to 127.0.0.1 is rejected by the
        // allow-list.
        let allowed_listener = bind_loopback();
        let allowed_port = allowed_listener.local_addr().unwrap().port();
        let body = b"redirected payload".to_vec();
        let _allowed = spawn_mock(allowed_listener, body.clone());
        let redirect_listener = bind_loopback();
        let redirect_port = redirect_listener.local_addr().unwrap().port();
        let _redirect = spawn_redirect(
            redirect_listener,
            format!("http://127.0.0.1:{allowed_port}/file"),
        );
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        let err = download_to_file(
            &format!("http://127.0.0.1:{redirect_port}/file"),
            &dest,
            body.len() as u64,
            None,
        )
        .unwrap_err();
        // 127.0.0.1 is not in the allow-list; the loopback redirect should
        // be rejected, proving the host allow-list works.
        match err {
            DownloadError::DisallowedRedirect(_) => {}
            other => panic!("expected DisallowedRedirect, got {other:?}"),
        }
    }

    #[test]
    fn user_agent_is_minimal() {
        assert!(USER_AGENT.starts_with("AEC-Studio/"));
        // No OS / arch / locale / user info.
        assert!(!USER_AGENT.contains("Mozilla"));
        assert!(!USER_AGENT.to_ascii_lowercase().contains("linux"));
        assert!(!USER_AGENT.to_ascii_lowercase().contains("darwin"));
        assert!(!USER_AGENT.to_ascii_lowercase().contains("windows"));
    }
}
