//! Sidecar transport. Wraps [`crate::http`] with the request/response shapes
//! expected by `llama-server`'s `/completion` endpoint (the same shape used by
//! upstream llama.cpp / PrismML).
//!
//! Reference for the JSON envelope:
//! <https://github.com/ggerganov/llama.cpp/blob/master/examples/server/README.md#api-endpoints>.
//!
//! We deliberately only support the synchronous (non-streaming) shape — every
//! Phase 1 AI tool returns a single GBNF-constrained JSON envelope, so there
//! is no benefit to consuming a token-by-token SSE stream.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::http::{self, HttpError};

/// Conservative defaults for completion sampling. The grammar is what really
/// constrains the output; temperature is set low so that valid tool-call
/// envelopes are emitted reliably across model sizes.
pub const DEFAULT_TEMPERATURE: f32 = 0.2;
pub const DEFAULT_TOP_P: f32 = 0.9;
pub const DEFAULT_MAX_PREDICT: u32 = 1024;
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_millis(750);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("http: {0}")]
    Http(#[from] HttpError),
    #[error("encode request: {0}")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("decode response: {0}")]
    DecodeResponse(#[source] serde_json::Error),
    #[error("sidecar returned an empty completion")]
    EmptyCompletion,
}

/// Subset of the llama.cpp `/completion` request envelope. Anything we don't
/// set explicitly inherits the server's defaults from `workers/ai/config.json`.
#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
    pub prompt: String,
    pub grammar: String,
    #[serde(rename = "n_predict")]
    pub max_predict: u32,
    pub temperature: f32,
    pub top_p: f32,
    /// `cache_prompt = true` lets llama-server reuse KV-cache across requests
    /// that share a prefix, which is the common case for our tool prompts.
    pub cache_prompt: bool,
    /// We never want the server to stream — see module doc.
    pub stream: bool,
}

impl CompletionRequest {
    pub fn new(prompt: impl Into<String>, grammar: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            grammar: grammar.into(),
            max_predict: DEFAULT_MAX_PREDICT,
            temperature: DEFAULT_TEMPERATURE,
            top_p: DEFAULT_TOP_P,
            cache_prompt: true,
            stream: false,
        }
    }

    pub fn with_max_predict(mut self, value: u32) -> Self {
        self.max_predict = value;
        self
    }

    pub fn with_temperature(mut self, value: f32) -> Self {
        self.temperature = value;
        self
    }
}

/// Subset of llama.cpp's `/completion` response. The model's tool-call JSON
/// is in `content`; the rest of the fields are informational and surfaced
/// via the runtime's audit log.
#[derive(Debug, Clone, Deserialize)]
pub struct CompletionResponse {
    /// The raw, GBNF-constrained string the model produced.
    pub content: String,
    /// llama.cpp's `stopped_*` family flatten down to a single boolean
    /// "did the model finish cleanly".
    #[serde(default)]
    pub stop: bool,
    /// Number of tokens predicted (returned by llama.cpp as `tokens_predicted`
    /// or `predicted_tokens` depending on server version; both are alias'd).
    #[serde(default, alias = "predicted_tokens", alias = "tokens_predicted")]
    pub tokens_predicted: u32,
    /// Why the model stopped — `"stop"`, `"limit"`, `"eos"`, etc. Optional
    /// because older servers do not return it.
    #[serde(default)]
    pub stopped_reason: Option<String>,
}

/// Thin transport handle. Holds the loopback port + per-request timeout so
/// the planner doesn't have to re-read it from `RuntimeConfig` every call.
#[derive(Debug, Clone)]
pub struct SidecarTransport {
    port: u16,
    request_timeout: Duration,
}

impl SidecarTransport {
    pub fn new(port: u16, request_timeout: Duration) -> Self {
        Self {
            port,
            request_timeout,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Probe the sidecar's `/health` endpoint. Returns `Ok(true)` only on
    /// a 2xx response whose body parses as `{"status":"ok"}` (the shape
    /// llama-server emits when the model is warm). Any other response —
    /// including `503 {"status":"loading model"}` during boot — returns
    /// `Ok(false)` so the runtime can stay in `Loading`.
    pub fn health(&self) -> Result<bool, TransportError> {
        match http::request(
            self.port,
            "GET",
            "/health",
            "",
            DEFAULT_CONNECT_TIMEOUT,
            DEFAULT_HEALTH_TIMEOUT,
        ) {
            Ok(resp) => Ok(is_health_ok(&resp.body)),
            Err(HttpError::HttpStatus { status: 503, .. }) => Ok(false),
            Err(HttpError::Connect { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Submit a completion. Blocks until the sidecar returns; the caller's
    /// `Mutex<AiState>` ensures only one completion is in flight at a time.
    pub fn complete(
        &self,
        request: &CompletionRequest,
    ) -> Result<CompletionResponse, TransportError> {
        let body = serde_json::to_string(request).map_err(TransportError::EncodeRequest)?;
        let resp = http::request(
            self.port,
            "POST",
            "/completion",
            &body,
            DEFAULT_CONNECT_TIMEOUT,
            self.request_timeout,
        )?;
        let parsed: CompletionResponse =
            serde_json::from_str(&resp.body).map_err(TransportError::DecodeResponse)?;
        if parsed.content.is_empty() {
            return Err(TransportError::EmptyCompletion);
        }
        Ok(parsed)
    }
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
    fn completion_request_serializes_as_llama_cpp_expects() {
        let req = CompletionRequest::new("hello", "root ::= \"x\"")
            .with_max_predict(64)
            .with_temperature(0.1);
        let body = serde_json::to_string(&req).unwrap();
        // Pin the field names — llama.cpp's /completion endpoint is
        // documented to consume these exact keys.
        assert!(body.contains("\"prompt\":\"hello\""));
        assert!(body.contains("\"grammar\":"));
        assert!(body.contains("\"n_predict\":64"));
        assert!(body.contains("\"temperature\":0.1"));
        assert!(body.contains("\"cache_prompt\":true"));
        assert!(body.contains("\"stream\":false"));
    }

    #[test]
    fn completion_response_accepts_both_alias_keys() {
        let body_a = r#"{"content":"ok","tokens_predicted":12,"stop":true}"#;
        let body_b = r#"{"content":"ok","predicted_tokens":34,"stop":true}"#;
        let a: CompletionResponse = serde_json::from_str(body_a).unwrap();
        let b: CompletionResponse = serde_json::from_str(body_b).unwrap();
        assert_eq!(a.tokens_predicted, 12);
        assert_eq!(b.tokens_predicted, 34);
    }

    #[test]
    fn health_ok_recognises_status_ok() {
        assert!(is_health_ok(r#"{"status":"ok"}"#));
        assert!(!is_health_ok(r#"{"status":"loading model"}"#));
        assert!(!is_health_ok("not json"));
        assert!(!is_health_ok(""));
    }
}
