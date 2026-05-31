//! Image-gen HTTP transport.
//!
//! Wraps [`crate::http`] with the request / response shapes the
//! image-gen sidecar exposes. Today that is leejet's
//! `stable-diffusion.cpp` `sd-server`, which speaks the AUTOMATIC1111
//! WebUI subset (`POST /sdapi/v1/txt2img`); the bridge surface
//! deliberately abstracts over the wire format so a future native
//! bonsai-image server can be slotted in by replacing only this file.
//!
//! Cancellation is via [`crate::AiCancelToken`] — the same token type
//! the text transport uses, so the bridge can cancel a long-running
//! txt2img and a streaming completion through a single primitive.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::http::{self, HttpError};
use crate::transport::AiCancelToken;

/// Default per-request timeout. Diffusion sampling at 1024×1024 / 50
/// steps takes ~30–90 s on commodity GPUs and 2–6 minutes on CPU, so
/// the budget is much larger than the text transport's request
/// timeout (which is bounded by `max_predict * per-token`). The
/// governor narrows this for low-tier hosts.
pub const DEFAULT_IMAGE_GEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Health probe budget. The image-gen `/health` endpoint is a tiny
/// JSON response (`{"status":"ok"}`) so it should resolve well under a
/// second even when the model is warming.
pub const DEFAULT_IMAGE_GEN_HEALTH_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Connect-side timeout, shared with the text transport's value so
/// both sidecars surface "binary failed to bind" failures within the
/// same window.
pub const DEFAULT_IMAGE_GEN_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Conservative default image dimensions. The renderer will narrow
/// these per governor tier, but a 512×512 / 20-step image at default
/// CFG is the sanity-check shape that every backend supports.
pub const DEFAULT_IMAGE_GEN_WIDTH: u32 = 512;
pub const DEFAULT_IMAGE_GEN_HEIGHT: u32 = 512;
pub const DEFAULT_IMAGE_GEN_STEPS: u32 = 20;
pub const DEFAULT_IMAGE_GEN_CFG_SCALE: f32 = 7.0;

/// Hard ceiling on the encoded image byte budget — 16 MiB. A 1024×
/// 1024 RGB PNG max-quality is < 5 MiB, so 16 MiB is a wide safety
/// margin while still bounding sidecar misbehaviour. The body parser
/// in `crate::http` enforces a smaller default; we raise it here only
/// for the image-gen channel.
pub const MAX_IMAGE_GEN_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ImageGenTransportError {
    #[error("http: {0}")]
    Http(#[from] HttpError),
    #[error("encode request: {0}")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("decode response: {0}")]
    DecodeResponse(#[source] serde_json::Error),
    #[error("image-gen sidecar returned no image bytes")]
    EmptyImage,
    #[error("image-gen sidecar returned a non-base64 payload: {0}")]
    InvalidBase64(String),
    #[error("request cancelled by caller")]
    Cancelled,
}

/// Subset of the AUTOMATIC1111 `txt2img` request envelope that
/// `stable-diffusion.cpp` accepts. Any field we don't set explicitly
/// inherits the server's default.
#[derive(Debug, Clone, Serialize)]
pub struct ImageGenRequest {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    /// `None` means "let the sidecar pick a random seed and surface it
    /// in the response". The renderer pins the returned seed back into
    /// the request UI so the user can reproduce a result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Sampler name. `"euler_a"` is the conservative default that
    /// every SD-family backend supports; `"dpmpp_2m"` is preferred for
    /// 20-step SDXL-turbo / Flux-schnell distillations. A future
    /// bonsai-image server may extend the set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampler_name: Option<String>,
    /// Number of samples per request. We always send 1 — multi-sample
    /// batching is governor-level (rate-limited per minute).
    pub batch_size: u32,
}

impl ImageGenRequest {
    /// Build a request from a prompt + the negotiated governor
    /// dimensions. Other fields take the conservative defaults
    /// declared at the top of this module.
    pub fn new(prompt: impl Into<String>, width: u32, height: u32, steps: u32) -> Self {
        Self {
            prompt: prompt.into(),
            negative_prompt: None,
            width,
            height,
            steps,
            cfg_scale: DEFAULT_IMAGE_GEN_CFG_SCALE,
            seed: None,
            sampler_name: Some("euler_a".into()),
            batch_size: 1,
        }
    }

    pub fn with_negative_prompt(mut self, value: impl Into<String>) -> Self {
        self.negative_prompt = Some(value.into());
        self
    }

    pub fn with_seed(mut self, value: i64) -> Self {
        self.seed = Some(value);
        self
    }

    pub fn with_cfg_scale(mut self, value: f32) -> Self {
        self.cfg_scale = value;
        self
    }

    pub fn with_sampler(mut self, name: impl Into<String>) -> Self {
        self.sampler_name = Some(name.into());
        self
    }
}

/// Subset of the AUTOMATIC1111 `txt2img` response. We only consume
/// the first image; multi-image responses (batch_size > 1) are
/// rejected upstream by [`ImageGenRequest::batch_size`] = 1.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenResponseRaw {
    /// Base64-encoded PNG bytes. AUTOMATIC1111 returns a non-empty
    /// array; we take `images[0]`.
    #[serde(default)]
    pub images: Vec<String>,
    /// Echoed parameters (`{"seed": 42, ...}`). Opaque JSON — we only
    /// peek at `seed`.
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// Free-form info string with sampling details (model, sampler,
    /// timings). Surfaced via the audit log for debugging.
    #[serde(default)]
    pub info: Option<String>,
}

/// Normalised image-gen response with the base64 decoded into raw PNG
/// bytes. The bridge re-encodes as a data URL for the renderer.
#[derive(Debug, Clone)]
pub struct ImageGenResponse {
    pub png_bytes: Vec<u8>,
    pub seed: Option<i64>,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub info: Option<String>,
}

/// Thin transport handle. Holds the loopback port + per-request timeout
/// so the runtime doesn't have to re-read them from `ImageGenConfig`
/// every call.
#[derive(Debug, Clone)]
pub struct ImageGenTransport {
    port: u16,
    request_timeout: Duration,
}

impl ImageGenTransport {
    pub fn new(port: u16, request_timeout: Duration) -> Self {
        Self {
            port,
            request_timeout,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Probe the sidecar's `/health` endpoint. Returns `Ok(true)` only
    /// on a 2xx response whose body parses as `{"status":"ok"}` —
    /// stable-diffusion.cpp's `sd-server` and the planned native
    /// bonsai-image server both expose this shape. `503` and connect
    /// failures fold to `Ok(false)` so the runtime stays in `Loading`.
    pub fn health(&self) -> Result<bool, ImageGenTransportError> {
        match http::request(
            self.port,
            "GET",
            "/health",
            "",
            DEFAULT_IMAGE_GEN_CONNECT_TIMEOUT,
            DEFAULT_IMAGE_GEN_HEALTH_TIMEOUT,
        ) {
            Ok(resp) => Ok(is_health_ok(&resp.body)),
            Err(HttpError::HttpStatus { status: 503, .. }) => Ok(false),
            Err(HttpError::Connect { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Submit a txt2img request. Blocks until the sidecar returns; the
    /// runtime's [`ImageGenRuntime`](super::runtime::ImageGenRuntime)
    /// serialises requests through a `Mutex<Option<ImageGenHandle>>`
    /// so concurrent renderer calls do not share a `TcpStream`.
    ///
    /// `cancel` is checked once before the request goes on the wire
    /// and once after the response arrives. Mid-request cancellation
    /// is implemented by dropping the underlying `TcpStream` from the
    /// runtime — this method itself is synchronous and does not poll
    /// the token mid-request.
    pub fn generate(
        &self,
        request: &ImageGenRequest,
        cancel: Option<&AiCancelToken>,
    ) -> Result<ImageGenResponse, ImageGenTransportError> {
        if cancel.is_some_and(AiCancelToken::is_cancelled) {
            return Err(ImageGenTransportError::Cancelled);
        }
        let body = serde_json::to_string(request).map_err(ImageGenTransportError::EncodeRequest)?;
        let resp = http::request_with_body_limit(
            self.port,
            "POST",
            "/sdapi/v1/txt2img",
            &body,
            DEFAULT_IMAGE_GEN_CONNECT_TIMEOUT,
            self.request_timeout,
            MAX_IMAGE_GEN_BODY_BYTES,
        )?;
        if cancel.is_some_and(AiCancelToken::is_cancelled) {
            return Err(ImageGenTransportError::Cancelled);
        }
        let raw: ImageGenResponseRaw =
            serde_json::from_str(&resp.body).map_err(ImageGenTransportError::DecodeResponse)?;
        let first = raw
            .images
            .first()
            .ok_or(ImageGenTransportError::EmptyImage)?;
        let png_bytes = base64_decode(first)
            .map_err(|e| ImageGenTransportError::InvalidBase64(e.to_string()))?;
        let seed = raw
            .parameters
            .get("seed")
            .and_then(serde_json::Value::as_i64);
        Ok(ImageGenResponse {
            png_bytes,
            seed,
            width: request.width,
            height: request.height,
            steps: request.steps,
            info: raw.info,
        })
    }
}

/// Tiny zero-dep base64 decoder for the subset of payloads
/// stable-diffusion.cpp emits: standard alphabet (`A-Za-z0-9+/`),
/// optional `=` padding, no whitespace, no URL-safe variant. We pull
/// this in inline to avoid a runtime dep on the `base64` crate just
/// for one decode path; the encoder is unnecessary because we only
/// ever decode from the sidecar.
fn base64_decode(s: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf = [0u8; 4];
    let mut buf_len = 0usize;
    for (idx, &c) in bytes.iter().enumerate() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                // Padding only valid in the last 1-2 positions.
                if idx < bytes.len() - 2 {
                    return Err(Base64Error::UnexpectedPadding);
                }
                continue;
            }
            other => return Err(Base64Error::InvalidByte(other)),
        };
        buf[buf_len] = v;
        buf_len += 1;
        if buf_len == 4 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
            out.push((buf[2] << 6) | buf[3]);
            buf_len = 0;
        }
    }
    match buf_len {
        0 => {}
        2 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
        }
        3 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        _ => return Err(Base64Error::IncompleteQuartet),
    }
    Ok(out)
}

#[derive(Debug, Error)]
enum Base64Error {
    #[error("invalid base64 byte: 0x{0:02x}")]
    InvalidByte(u8),
    #[error("padding `=` before end of input")]
    UnexpectedPadding,
    #[error("input length not a multiple of 4 base64 chars (after padding)")]
    IncompleteQuartet,
}

fn is_health_ok(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    v.get("status").and_then(|s| s.as_str()) == Some("ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_gen_request_serializes_with_canonical_field_names() {
        let req = ImageGenRequest::new("a wooden chair, studio lighting", 512, 512, 20)
            .with_negative_prompt("blurry, low quality")
            .with_seed(42)
            .with_cfg_scale(7.5)
            .with_sampler("dpmpp_2m");
        let body = serde_json::to_string(&req).unwrap();
        // Pin the AUTOMATIC1111-shaped field names that stable-diffusion.cpp consumes.
        assert!(body.contains("\"prompt\":\"a wooden chair, studio lighting\""));
        assert!(body.contains("\"negative_prompt\":\"blurry, low quality\""));
        assert!(body.contains("\"width\":512"));
        assert!(body.contains("\"height\":512"));
        assert!(body.contains("\"steps\":20"));
        assert!(body.contains("\"cfg_scale\":7.5"));
        assert!(body.contains("\"seed\":42"));
        assert!(body.contains("\"sampler_name\":\"dpmpp_2m\""));
        assert!(body.contains("\"batch_size\":1"));
    }

    #[test]
    fn image_gen_request_omits_optional_fields_when_none() {
        let req = ImageGenRequest::new("test", 768, 768, 30);
        let body = serde_json::to_string(&req).unwrap();
        assert!(body.contains("\"prompt\":\"test\""));
        // None-valued options drop out of the wire format via skip_serializing_if.
        assert!(!body.contains("\"negative_prompt\""));
        assert!(!body.contains("\"seed\""));
        // sampler_name has a Some default so it always serializes.
        assert!(body.contains("\"sampler_name\":\"euler_a\""));
    }

    #[test]
    fn health_ok_recognises_status_ok() {
        assert!(is_health_ok(r#"{"status":"ok"}"#));
        assert!(!is_health_ok(r#"{"status":"loading model"}"#));
        assert!(!is_health_ok("not json"));
        assert!(!is_health_ok(""));
    }

    #[test]
    fn base64_decode_round_trips_a_known_png_signature() {
        // The first 8 bytes of every PNG are 0x89 50 4E 47 0D 0A 1A 0A
        // (`%PNG\r\n\x1a\n`). Their base64 encoding is `iVBORw0KGgo=`.
        let decoded = base64_decode("iVBORw0KGgo=").unwrap();
        assert_eq!(
            decoded,
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }

    #[test]
    fn base64_decode_accepts_unpadded_input() {
        // "Man" → "TWFu" (no padding). Multi-byte plain.
        let decoded = base64_decode("TWFu").unwrap();
        assert_eq!(decoded, b"Man");
    }

    #[test]
    fn base64_decode_rejects_invalid_byte() {
        let err = base64_decode("AA!A").unwrap_err();
        assert!(matches!(err, Base64Error::InvalidByte(b'!')));
    }

    #[test]
    fn generate_returns_cancelled_when_token_set_before_request() {
        // The cancel-before-wire branch does not need a live sidecar.
        let transport = ImageGenTransport::new(1, Duration::from_millis(100));
        let cancel = AiCancelToken::new();
        cancel.cancel();
        let req = ImageGenRequest::new("test", 512, 512, 20);
        let result = transport.generate(&req, Some(&cancel));
        assert!(matches!(result, Err(ImageGenTransportError::Cancelled)));
    }
}
