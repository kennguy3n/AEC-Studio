//! Integration tests for the bridge's six `ai_*` service methods.
//!
//! These tests stand up a real `BridgeService` and inject a mock-sidecar
//! `AiState` so we can exercise the full `ai_plan` -> safety_validator
//! -> diff_engine -> pending diff lifecycle path end-to-end without
//! spawning a real `llama-server`. The mock sidecar is a tiny
//! `TcpListener` on a random loopback port that returns canned
//! `/completion` responses; the bridge's `AiState::__test_with_transport`
//! adopts it as if it were a freshly-spawned child.
//!
//! Why this lives in `tests/` rather than inline `#[cfg(test)]`:
//!   * It depends on a tokio-free mock HTTP server that wants to spawn
//!     OS threads, which we'd rather not bring into the service.rs unit
//!     scope.
//!   * It pins the *public* `ai_*` surface (the methods the napi shim
//!     calls) — keeping it in `tests/` ensures the API is observable
//!     without the napi feature flag turned on.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use aec_ai::{RuntimeConfig, SidecarTransport};
use aec_bridge::{ai_state::AiState, BridgeConfig, BridgeService};
use aec_core::Scope;
use tempfile::TempDir;

fn make_service() -> (BridgeService, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [13u8; 32]).unwrap();
    (s, tmp)
}

fn bind_loopback() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind loopback")
}

/// Spawn a mock sidecar that handles `count` requests with the same
/// canned response. The caller picks `count` large enough to cover
/// every `ai_plan` call the test makes; extras are silently dropped.
fn spawn_mock_sidecar(
    listener: TcpListener,
    response: &'static [u8],
    count: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .ok();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
            stream.flush().ok();
        }
    })
}

fn wire_ai_state_to_mock(service: &BridgeService, port: u16) {
    // Use the test-only helper to swap in an AiState that's pre-attached
    // to the mock TCP server — no real `llama-server` spawn.
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let state = AiState::__test_with_transport(RuntimeConfig::default(), transport);
    service.__test_install_ai_state(state).unwrap();
}

/// Wire-format HTTP response for a successful style-assistant
/// completion. Built dynamically so the Content-Length stays in sync
/// with the body bytes; consumers leak it as `'static` via
/// [`canned_response`].
fn valid_style_assistant_response_bytes() -> Vec<u8> {
    let body = br#"{"content":"{\"furniture_ids\":[\"a\",\"b\"],\"material_ids\":[\"c\"],\"lighting_preset_id\":\"warm_evening\"}","stop":true,"tokens_predicted":42}"#;
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn canned_response(bytes: Vec<u8>) -> &'static [u8] {
    Box::leak(bytes.into_boxed_slice())
}

#[test]
fn ai_list_tools_returns_alphabetical_descriptors() {
    let (s, _g) = make_service();
    let tools = s.ai_list_tools().expect("ai_list_tools");
    assert!(
        !tools.is_empty(),
        "the default tool registry must surface at least one tool"
    );
    // The list is sorted by `name`, which is the lowercase snake_case
    // identifier. Spot-check the ordering invariant.
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "ai_list_tools must be sorted by name");

    // Every tool's `allowed_scopes` must be a non-empty subset of the
    // five canonical scopes — if this fails, the wire shape drifted.
    for t in &tools {
        assert!(
            !t.allowed_scopes.is_empty(),
            "tool {} must have at least one allowed scope",
            t.name
        );
        for sc in &t.allowed_scopes {
            assert!(
                matches!(
                    sc.as_str(),
                    "design" | "draft" | "bim" | "render" | "deliver"
                ),
                "unexpected scope `{sc}` on tool {}",
                t.name
            );
        }
    }
}

#[test]
fn ai_runtime_status_reports_idle_by_default() {
    let (s, _g) = make_service();
    let st = s.ai_runtime_status().unwrap();
    // Lazy spawn — until first ai_plan call the runtime is idle.
    assert_eq!(st.state, "idle");
    assert!(st.last_error.is_none());
    assert!(st.pending_diff_ids.is_empty());
}

#[test]
fn ai_plan_dispatches_through_mock_sidecar_and_registers_diff() {
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&s, port);

    let result = s
        .ai_plan("style_assistant", Scope::Design, "a warm evening", "{}", 5)
        .expect("ai_plan should round-trip through the mock sidecar");

    assert!(!result.diff_id.is_empty());
    assert!(result.diff_id.starts_with("diff_"));
    assert_eq!(result.tool, "style_assistant");
    assert!(result.parsed.get("furniture_ids").is_some());
    assert!(result.parsed.get("material_ids").is_some());
    assert!(result.parsed.get("lighting_preset_id").is_some());

    // The runtime is now Ready and the pending diff list contains
    // the just-created diff id.
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(st.state, "ready");
    assert_eq!(st.pending_diff_ids, vec![result.diff_id.clone()]);

    join.join().ok();
}

#[test]
fn ai_plan_rejects_unknown_tool() {
    let (s, _g) = make_service();
    let err = s
        .ai_plan("not_a_real_tool", Scope::Design, "", "{}", 1)
        .unwrap_err();
    assert!(err.to_string().contains("unknown ai tool"));
}

#[test]
fn ai_plan_rejects_malformed_context_json() {
    let (s, _g) = make_service();
    let err = s
        .ai_plan("style_assistant", Scope::Design, "", "not json {{{", 1)
        .unwrap_err();
    assert!(err.to_string().contains("context_json"), "got error: {err}");
}

#[test]
fn ai_accept_diff_removes_pending_diff() {
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&s, port);

    let result = s
        .ai_plan("style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let outcome = s.ai_accept_diff(&result.diff_id).unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.diff_id, result.diff_id);

    // After accept, the diff is no longer pending.
    let st = s.ai_runtime_status().unwrap();
    assert!(st.pending_diff_ids.is_empty());

    // Re-accepting the same id is an error (already removed).
    let err = s.ai_accept_diff(&result.diff_id).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn ai_reject_diff_removes_pending_diff() {
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&s, port);

    let result = s
        .ai_plan("style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let outcome = s.ai_reject_diff(&result.diff_id).unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.diff_id, result.diff_id);

    let st = s.ai_runtime_status().unwrap();
    assert!(st.pending_diff_ids.is_empty());
}

#[test]
fn ai_accept_diff_rejects_unknown_id() {
    let (s, _g) = make_service();
    let err = s.ai_accept_diff("diff_does_not_exist").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn ai_accept_diff_rejects_malformed_id() {
    let (s, _g) = make_service();
    // Missing the `diff_` prefix — DiffId::from_string rejects it.
    let err = s.ai_accept_diff("not-a-diff-id").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn ai_cancel_job_resets_runtime_to_idle() {
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&s, port);

    // Drive the runtime to Ready via a successful plan.
    let _ = s
        .ai_plan("style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    assert_eq!(s.ai_runtime_status().unwrap().state, "ready");

    let r = s.ai_cancel_job("job_anything").unwrap();
    assert!(r.cancelled);

    // After cancel, the runtime is back to Idle and the handle is gone.
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(st.state, "idle");
}

#[test]
fn ai_cancel_job_is_idempotent_when_no_sidecar_running() {
    let (s, _g) = make_service();
    // No ai_plan ever called → no sidecar spawned. cancel must still
    // succeed (idempotent).
    let r = s.ai_cancel_job("anything").unwrap();
    assert!(r.cancelled);
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(st.state, "idle");
}

#[test]
fn ai_plan_routes_a_failed_safety_validation_back_as_ai_error() {
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();

    // Content is grammatically wrong (missing required keys) → safety
    // validator rejects.
    let body = br#"{"content":"{\"unrelated\":\"junk\"}","stop":true,"tokens_predicted":3}"#;
    let response_str = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut bytes = response_str.into_bytes();
    bytes.extend_from_slice(body);
    let response: &'static [u8] = bytes.leak();
    let _join = spawn_mock_sidecar(listener, response, 1);
    wire_ai_state_to_mock(&s, port);

    let err = s
        .ai_plan("style_assistant", Scope::Design, "", "{}", 5)
        .unwrap_err();
    // Safety errors come back as BridgeServiceError::Ai (the planner
    // wraps the SafetyError in a PlanError, which `From` converts into
    // `BridgeServiceError::Ai`).
    let msg = err.to_string();
    assert!(msg.starts_with("ai:"), "got: {msg}");
}

#[test]
fn ai_plan_serialises_to_finite_json_numbers() {
    // Defense against an `f64::INFINITY` footgun (we don't have any
    // floats today, but adding the test pins the contract for future
    // changes that introduce score/confidence fields). The current
    // shape only has strings + an integer, so serialisation cannot
    // fail; once we add a confidence field, this test catches a
    // regression.
    let (s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&s, port);

    let result = s
        .ai_plan("style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let json = serde_json::to_string(&result).expect("AiPlanResult must serialise");
    assert!(!json.contains("inf"));
    assert!(!json.contains("NaN"));
}
