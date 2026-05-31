//! Minimal HTTP/1.1 client for talking to the local llama-server (PrismML)
//! sidecar over loopback.
//!
//! We deliberately do NOT pull in a full HTTP library (ureq / reqwest / hyper)
//! here, for three reasons:
//!
//!   1. **Loopback only.** The sidecar listens on `127.0.0.1:<port>` and the
//!      AI safety posture forbids any outbound traffic from tool calls. We
//!      never need DNS, TLS, redirects, or cookies — so a 100-line
//!      `std::net::TcpStream` client is a strictly smaller attack surface
//!      than a general-purpose HTTP library.
//!   2. **Separate from `model_download.rs`.** Model downloads from the
//!      public internet (HuggingFace) need TLS + redirect handling, and use
//!      `ureq + rustls` in `crate::model_download`. That path runs only at
//!      install time and never during inference; this loopback path runs on
//!      every tool call and stays minimal.
//!   3. **Testability.** Every byte that goes on the wire is visible in this
//!      file, so the `tests/sidecar_mock.rs` integration test can pin the
//!      exact request line and headers against a tiny `TcpListener` mock
//!      without depending on an HTTP library's internal request serializer.
//!
//! The transport is **synchronous and blocking**. Callers serialize access
//! through the bridge's `Mutex<AiState>` so concurrent renderer calls (e.g.
//! a long `ai_plan` racing a `ai_runtime_status` poll) don't share a
//! TcpStream.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use thiserror::Error;

/// Default maximum response body size we accept from the sidecar. The
/// text sidecar emits a single tool-call JSON envelope (typically
/// < 8 KiB); 1 MiB is a wide safety margin that still bounds memory
/// if the sidecar misbehaves.
///
/// The text [`request`] function uses this default. Callers with
/// larger payloads (notably the Phase 18 Group C image-gen sidecar,
/// which returns multi-MiB base64 PNGs from the AUTOMATIC1111
/// `/sdapi/v1/txt2img` endpoint) call [`request_with_body_limit`] with an
/// explicit budget instead of widening this default for everyone.
pub const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("tcp connect to {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("write request: {0}")]
    Write(#[source] std::io::Error),
    #[error("read response: {0}")]
    Read(#[source] std::io::Error),
    #[error("invalid status line `{0}`")]
    InvalidStatusLine(String),
    #[error("sidecar returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("response body exceeded {limit} bytes")]
    BodyTooLarge { limit: usize },
    #[error("malformed response headers")]
    MalformedHeaders,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Issue a single HTTP request to `127.0.0.1:port` with the default
/// 1 MiB response-body cap. See [`request_with_body_limit`] for the
/// variant that accepts an explicit limit.
///
/// `path` must already include any leading `/`. `method` is
/// `"GET"` or `"POST"`. `body` is the JSON envelope (or empty for
/// GET).
///
/// The request is hard-coded to:
///   * `Host: 127.0.0.1:<port>`
///   * `Connection: close`
///   * `Content-Type: application/json` (when `body` is non-empty)
///
/// `connect_timeout` bounds the initial TCP handshake; `io_timeout`
/// applies to every read/write on the stream once connected.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    request_with_body_limit(
        port,
        method,
        path,
        body,
        connect_timeout,
        io_timeout,
        MAX_RESPONSE_BODY_BYTES,
    )
}

/// Same as [`request`] but with a caller-supplied response-body
/// `limit` (in bytes). Used by the Phase 18 image-gen transport,
/// which expects multi-MiB base64 PNG payloads from the
/// AUTOMATIC1111 `/sdapi/v1/txt2img` endpoint — widening the
/// default 1 MiB cap globally would soften the memory bound on
/// every other loopback caller (`/v1/chat/completions`,
/// `/health`, …), so the limit is per-call here.
///
/// The limit is applied in three places: the `Content-Length`
/// pre-check, the chunked-transfer accumulator, and the
/// `Connection: close` EOF tail. Exceeding it returns
/// [`HttpError::BodyTooLarge`] without consuming the whole stream
/// — the bound is hard, not advisory.
pub fn request_with_body_limit(
    port: u16,
    method: &str,
    path: &str,
    body: &str,
    connect_timeout: Duration,
    io_timeout: Duration,
    limit: usize,
) -> Result<HttpResponse, HttpError> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut stream = TcpStream::connect_timeout(&addr, connect_timeout)
        .map_err(|source| HttpError::Connect { addr, source })?;
    // Map each setup error to the side it governs (read vs write)
    // so the user-facing message tells the truth about which channel
    // the platform refused to configure. A zero-duration timeout on
    // some platforms (e.g. Windows pre-Vista) returns EINVAL here
    // and `"write request: ..."` for a read-timeout failure would be
    // genuinely confusing during debug.
    stream
        .set_read_timeout(Some(io_timeout))
        .map_err(HttpError::Read)?;
    stream
        .set_write_timeout(Some(io_timeout))
        .map_err(HttpError::Write)?;

    let mut req = String::with_capacity(256 + body.len());
    req.push_str(method);
    req.push(' ');
    req.push_str(path);
    req.push_str(" HTTP/1.1\r\n");
    req.push_str("Host: 127.0.0.1:");
    req.push_str(&port.to_string());
    req.push_str("\r\n");
    req.push_str("Connection: close\r\n");
    req.push_str("Accept: application/json\r\n");
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str("Content-Length: ");
        req.push_str(&body.len().to_string());
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(HttpError::Write)?;
    if !body.is_empty() {
        stream
            .write_all(body.as_bytes())
            .map_err(HttpError::Write)?;
    }
    stream.flush().map_err(HttpError::Write)?;

    let mut reader = BufReader::new(stream);
    parse_response(&mut reader, limit)
}

fn parse_response<R: Read + BufRead>(
    reader: &mut R,
    limit: usize,
) -> Result<HttpResponse, HttpError> {
    // Status line: e.g. "HTTP/1.1 200 OK\r\n"
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(HttpError::Read)?;
    let trimmed = status_line.trim_end_matches(['\r', '\n']);
    let mut parts = trimmed.splitn(3, ' ');
    let _version = parts
        .next()
        .ok_or_else(|| HttpError::InvalidStatusLine(trimmed.into()))?;
    let status_str = parts
        .next()
        .ok_or_else(|| HttpError::InvalidStatusLine(trimmed.into()))?;
    let status: u16 = status_str
        .parse()
        .map_err(|_| HttpError::InvalidStatusLine(trimmed.into()))?;

    // Headers until blank line. Track Content-Length so we know how much to read.
    let mut content_length: Option<usize> = None;
    let mut transfer_encoding_chunked = false;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(HttpError::Read)?;
        if n == 0 {
            return Err(HttpError::MalformedHeaders);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (k, v) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => return Err(HttpError::MalformedHeaders),
        };
        if k.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                v.parse::<usize>()
                    .map_err(|_| HttpError::MalformedHeaders)?,
            );
        } else if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
            transfer_encoding_chunked = true;
        }
    }

    // Body. Per RFC 7230 §3.3.3 rule 3, `Transfer-Encoding` takes
    // precedence over `Content-Length` when both are present (the
    // historical request-smuggling exploit class). The llama-server
    // sidecar never sends both today, but checking TE first is
    // defense-in-depth in case this client is ever reused against a
    // different loopback server.
    let body = if transfer_encoding_chunked {
        read_chunked(reader, limit)?
    } else if let Some(len) = content_length {
        if len > limit {
            return Err(HttpError::BodyTooLarge { limit });
        }
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).map_err(HttpError::Read)?;
        String::from_utf8(buf).map_err(|_| HttpError::MalformedHeaders)?
    } else {
        // Connection: close → read until EOF, bounded by the cap.
        let mut buf = Vec::new();
        let mut take = reader.take(limit as u64 + 1);
        take.read_to_end(&mut buf).map_err(HttpError::Read)?;
        if buf.len() > limit {
            return Err(HttpError::BodyTooLarge { limit });
        }
        String::from_utf8(buf).map_err(|_| HttpError::MalformedHeaders)?
    };

    if !(200..300).contains(&status) {
        return Err(HttpError::HttpStatus { status, body });
    }
    Ok(HttpResponse { status, body })
}

fn read_chunked<R: BufRead>(reader: &mut R, limit: usize) -> Result<String, HttpError> {
    let mut out = Vec::new();
    loop {
        let mut size_line = String::new();
        reader.read_line(&mut size_line).map_err(HttpError::Read)?;
        let size_str = size_line.trim_end_matches(['\r', '\n']);
        // Strip optional chunk extensions (";...").
        let size_str = size_str.split(';').next().unwrap_or("");
        let size =
            usize::from_str_radix(size_str.trim(), 16).map_err(|_| HttpError::MalformedHeaders)?;
        if size == 0 {
            // Discard trailing CRLF after the zero-chunk + any trailer headers.
            let mut trailer = String::new();
            loop {
                trailer.clear();
                let n = reader.read_line(&mut trailer).map_err(HttpError::Read)?;
                if n == 0 || trailer.trim().is_empty() {
                    break;
                }
            }
            break;
        }
        if out.len() + size > limit {
            return Err(HttpError::BodyTooLarge { limit });
        }
        let mut buf = vec![0u8; size];
        reader.read_exact(&mut buf).map_err(HttpError::Read)?;
        out.extend_from_slice(&buf);
        // Discard the trailing CRLF after the chunk data.
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf).map_err(HttpError::Read)?;
        if &crlf != b"\r\n" {
            return Err(HttpError::MalformedHeaders);
        }
    }
    String::from_utf8(out).map_err(|_| HttpError::MalformedHeaders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_simple_content_length_response() {
        // Content-Length: 11 = len(`{"ok":true}`)
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
        let mut reader = BufReader::new(Cursor::new(raw));
        let r = parse_response(&mut reader, MAX_RESPONSE_BODY_BYTES).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"ok\":true}");
    }

    #[test]
    fn parses_chunked_response() {
        // Each chunk is `<size_in_hex>\r\n<data_bytes>\r\n`; the final
        // 0-sized chunk terminates the body.
        //   chunk 1: 6 bytes -> "{\"ok\":"
        //   chunk 2: 5 bytes -> "true}"
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n{\"ok\":\r\n5\r\ntrue}\r\n0\r\n\r\n";
        let mut reader = BufReader::new(Cursor::new(raw));
        let r = parse_response(&mut reader, MAX_RESPONSE_BODY_BYTES).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"ok\":true}");
    }

    #[test]
    fn rejects_5xx_with_body_propagated() {
        let raw = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 18\r\n\r\n{\"error\":\"warming\"}";
        let mut reader = BufReader::new(Cursor::new(raw));
        let err = parse_response(&mut reader, MAX_RESPONSE_BODY_BYTES).unwrap_err();
        match err {
            HttpError::HttpStatus { status, body } => {
                assert_eq!(status, 503);
                assert!(body.contains("warming"));
            }
            other => panic!("expected HttpStatus, got {other:?}"),
        }
    }

    #[test]
    fn rejects_content_length_above_cap() {
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_RESPONSE_BODY_BYTES + 1
        );
        let mut reader = BufReader::new(Cursor::new(raw.into_bytes()));
        let err = parse_response(&mut reader, MAX_RESPONSE_BODY_BYTES).unwrap_err();
        assert!(matches!(err, HttpError::BodyTooLarge { .. }));
    }

    #[test]
    fn rejects_malformed_status_line() {
        let raw = b"NOT_HTTP\r\n";
        let mut reader = BufReader::new(Cursor::new(raw));
        let err = parse_response(&mut reader, MAX_RESPONSE_BODY_BYTES).unwrap_err();
        assert!(matches!(err, HttpError::InvalidStatusLine(_)));
    }

    /// Pin the new `limit` parameter: a Content-Length that would
    /// fit under the default 1 MiB cap but exceeds the caller's
    /// per-request budget must still surface `BodyTooLarge`. The
    /// Phase 18 image-gen transport calls
    /// [`request_with_body_limit`] with a 16 MiB ceiling for base64
    /// PNGs; this test pins that the limit threading actually
    /// reaches the body-size check, not just the function
    /// signature.
    #[test]
    fn caller_supplied_limit_overrides_default_cap() {
        // 1024-byte body, but caller passes a 256-byte limit.
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n".to_vec();
        raw.extend(std::iter::repeat_n(b'x', 1024));
        let mut reader = BufReader::new(Cursor::new(raw));
        let err = parse_response(&mut reader, 256).unwrap_err();
        match err {
            HttpError::BodyTooLarge { limit } => assert_eq!(limit, 256),
            other => panic!("expected BodyTooLarge {{ limit: 256 }}, got {other:?}"),
        }
    }

    /// And the dual: a body that fits the caller's wider limit
    /// (e.g. 2 MiB > default 1 MiB) is accepted. We keep the test
    /// modest (a 4 KiB body with an 8 KiB limit) so it runs in
    /// microseconds.
    #[test]
    fn caller_supplied_limit_admits_bodies_above_default_when_widened() {
        let payload = "x".repeat(4 * 1024);
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let mut reader = BufReader::new(Cursor::new(raw.into_bytes()));
        let r = parse_response(&mut reader, 8 * 1024).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body.len(), 4 * 1024);
    }

    /// Chunked-transfer accumulator must also honour the caller's
    /// limit, not just the default cap. Without threading the
    /// limit into `read_chunked`, a chunked response could bypass
    /// the per-request budget.
    #[test]
    fn chunked_transfer_honours_caller_limit() {
        // Single 1024-byte chunk; caller passes a 256-byte limit.
        let chunk = "x".repeat(1024);
        let raw = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            chunk.len(),
            chunk,
        );
        let mut reader = BufReader::new(Cursor::new(raw.into_bytes()));
        let err = parse_response(&mut reader, 256).unwrap_err();
        match err {
            HttpError::BodyTooLarge { limit } => assert_eq!(limit, 256),
            other => panic!("expected BodyTooLarge {{ limit: 256 }}, got {other:?}"),
        }
    }
}
