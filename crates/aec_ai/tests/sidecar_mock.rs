//! End-to-end test of the local LLM runtime against a mock HTTP sidecar.
//!
//! Spawns a `TcpListener` on a random loopback port, plays the role of a
//! `llama-server` `/completion` endpoint, and exercises the full path:
//!
//!     ToolPlanner::dispatch
//!       -> SidecarTransport::complete
//!         -> http::request (real TCP socket on loopback)
//!           -> mock server returns canned JSON
//!         -> CompletionResponse parsed
//!       -> SafetyValidator (real check)
//!       -> PlanResponse returned to caller
//!       -> DiffEngine::build produces real DiffOperations
//!
//! This is the test the bridge's `ai_plan` integration test will hook into
//! once the bridge napi surface lands; for now it pins the contract that
//! `dispatch` end-to-end works without a real llama-server binary on the
//! machine.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use aec_ai::{
    transport::SidecarTransport, DiffEngine, DiffOperation, GrammarRegistry, PlanRequest,
    ToolPlanner, ToolSchemaRegistry,
};
use aec_core::Scope;

/// Pick an unused loopback port by binding 127.0.0.1:0 and immediately
/// returning the bound listener.
fn bind_loopback() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind loopback")
}

/// Spawn a one-shot mock server: it accepts a single connection, reads the
/// request (we don't parse it; we only need to count it), and writes the
/// supplied canned response.
fn spawn_one_shot_server(listener: TcpListener, response: &'static [u8]) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().expect("accept");
        // Drain the request — we don't need to parse it. Use a short read
        // timeout so the thread doesn't block forever if the client misbehaves.
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .ok();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        stream.write_all(response).expect("write response");
        stream.flush().ok();
    })
}

/// Spawn a mock server that handles N requests with a sequence of canned
/// responses. The Nth response is used for the Nth connection.
fn spawn_multi_shot_server(
    listener: TcpListener,
    responses: Vec<&'static [u8]>,
) -> (mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _addr)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .ok();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
            stream.flush().ok();
            let _ = done_tx.send(());
        }
    });
    (done_rx, handle)
}

#[test]
fn transport_health_check_recognises_status_ok() {
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let response =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";
    let join = spawn_one_shot_server(listener, response);

    let transport = SidecarTransport::new(port, Duration::from_secs(2));
    let ok = transport.health().expect("health probe should succeed");
    assert!(ok);
    join.join().ok();
}

#[test]
fn transport_health_check_returns_false_when_status_loading() {
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let response =
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"status\":\"loading model\"}";
    let _join = spawn_one_shot_server(listener, response);

    let transport = SidecarTransport::new(port, Duration::from_secs(2));
    let ok = transport.health().expect("health probe should succeed");
    assert!(!ok);
}

#[test]
fn transport_health_check_returns_false_on_connection_refused() {
    // Bind then drop to get a known-closed port.
    let port = {
        let l = bind_loopback();
        l.local_addr().unwrap().port()
    };
    let transport = SidecarTransport::new(port, Duration::from_secs(1));
    let ok = transport.health().expect("connection-refused -> Ok(false)");
    assert!(!ok);
}

#[test]
fn planner_dispatch_runs_full_pipeline_through_mock_sidecar() {
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();

    // Canned completion response — content is a valid style_assistant
    // payload (matches the safety validator + grammar matcher).
    let content = r#"{\"furniture_ids\":[\"a\",\"b\"],\"material_ids\":[\"c\"],\"lighting_preset_id\":\"warm_evening\"}"#;
    let body = format!(r#"{{"content":"{content}","stop":true,"tokens_predicted":42}}"#);
    // Build the HTTP wire response.
    let response_str = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let response: &'static [u8] = Box::leak(response_str.into_boxed_str().into_boxed_bytes());
    let join = spawn_one_shot_server(listener, response);

    let schemas = ToolSchemaRegistry::defaults();
    let grammars = GrammarRegistry::defaults();
    let planner = ToolPlanner::new(&schemas, &grammars);
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let request = PlanRequest {
        tool: aec_ai::ToolName::StyleAssistant,
        scope: Scope::Design,
        prompt: "a warm evening".into(),
        context: serde_json::json!({"room":"living"}),
        max_entities_modified: 5,
    };
    let response = planner
        .dispatch(&request, &transport)
        .expect("dispatch should succeed");

    assert_eq!(response.tool, aec_ai::ToolName::StyleAssistant);
    assert!(response.parsed.get("furniture_ids").is_some());
    assert!(response.parsed.get("material_ids").is_some());
    assert!(response.parsed.get("lighting_preset_id").is_some());

    // Diff engine maps the parsed payload to a Diff that the command engine
    // can consume.
    let diff = DiffEngine::build(&response);
    assert!(diff.operations.iter().any(
        |op| matches!(op, DiffOperation::Insert { entity_kind, .. } if entity_kind == "furniture")
    ));

    join.join().ok();
}

#[test]
fn planner_dispatch_propagates_malformed_completion_as_safety_error() {
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();

    // Content is grammatically wrong (missing required keys for
    // style_assistant) — safety validator must reject.
    let body = r#"{"content":"{\"unrelated\":\"junk\"}","stop":true,"tokens_predicted":3}"#;
    let response_str = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let response: &'static [u8] = Box::leak(response_str.into_boxed_str().into_boxed_bytes());
    let _join = spawn_one_shot_server(listener, response);

    let schemas = ToolSchemaRegistry::defaults();
    let grammars = GrammarRegistry::defaults();
    let planner = ToolPlanner::new(&schemas, &grammars);
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let request = PlanRequest {
        tool: aec_ai::ToolName::StyleAssistant,
        scope: Scope::Design,
        prompt: String::new(),
        context: serde_json::json!({}),
        max_entities_modified: 5,
    };
    let err = planner
        .dispatch(&request, &transport)
        .expect_err("malformed content must fail safety validation");
    assert!(matches!(err, aec_ai::PlanError::Safety(_)));
}

#[test]
fn transport_completion_handles_sequential_calls() {
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();

    let body1 = r#"{"content":"{\"furniture_ids\":[],\"material_ids\":[],\"lighting_preset_id\":\"x\"}","stop":true,"tokens_predicted":1}"#;
    let body2 = r#"{"content":"{\"furniture_ids\":[\"y\"],\"material_ids\":[],\"lighting_preset_id\":\"x\"}","stop":true,"tokens_predicted":2}"#;
    let resp1: &'static [u8] = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body1.len(),
            body1
        )
        .into_boxed_str()
        .into_boxed_bytes(),
    );
    let resp2: &'static [u8] = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body2.len(),
            body2
        )
        .into_boxed_str()
        .into_boxed_bytes(),
    );
    let (_done_rx, _handle) = spawn_multi_shot_server(listener, vec![resp1, resp2]);

    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let req = aec_ai::CompletionRequest::new("hello", "root ::= \"x\"");
    let r1 = transport.complete(&req).expect("first call");
    let r2 = transport.complete(&req).expect("second call");
    assert_eq!(r1.tokens_predicted, 1);
    assert_eq!(r2.tokens_predicted, 2);
    // r1 and r2 are CompletionResponse — content is the un-escaped string the
    // model produced (a JSON envelope).
    assert!(r1.content.contains("furniture_ids"));
    assert!(r2.content.contains("\"y\""));
}
