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
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aec_ai::{RuntimeConfig, RuntimeState, SidecarTransport};
use aec_bridge::{ai_state::AiState, BridgeConfig, BridgeService};
use aec_core::Scope;
use tempfile::TempDir;

fn write_template(root: &std::path::Path) {
    let category_dir = root.join("interior");
    std::fs::create_dir_all(&category_dir).unwrap();
    let json = serde_json::json!({
        "template_id": "interior.studio",
        "name": "AI endpoints fixture",
        "description": "in-test fixture",
        "units": "mm",
        "region_defaults": {
            "EU": {"units": "mm", "standards": ["IFC4"]}
        },
        "rooms": [],
        "default_walls": {
            "exterior_thickness_mm": 250,
            "interior_thickness_mm": 100,
            "material": "wall_white"
        },
        "lighting_preset": "daylight",
        "asset_shelf": [],
        "camera_presets": []
    });
    std::fs::write(category_dir.join("studio.json"), json.to_string()).unwrap();
}

fn make_service() -> (BridgeService, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    write_template(&templates);
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [13u8; 32]).unwrap();
    (s, tmp)
}

/// Boot a service AND create a real on-disk project so the AI
/// accept/reject path has a SQLCipher target to write commands to.
/// Returns the bridge, the tempdir guard, and the project path.
fn make_service_with_project() -> (BridgeService, TempDir, String) {
    let (mut s, tmp) = make_service();
    let summary = s
        .project_create_from_template("interior.studio", "AI Endpoints Project")
        .expect("create project");
    let path = summary.path;
    (s, tmp, path)
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

fn wire_ai_state_to_mock(service: &mut BridgeService, port: u16) {
    // Use the test-only helper to swap in an AiState that's pre-attached
    // to the mock TCP server — no real `llama-server` spawn.
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let state = AiState::__test_with_transport(RuntimeConfig::default(), transport);
    service.__test_install_ai_state(state);
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
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(
            &project_path,
            "style_assistant",
            Scope::Design,
            "a warm evening",
            "{}",
            5,
        )
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
    let (s, _g, project_path) = make_service_with_project();
    let err = s
        .ai_plan(&project_path, "not_a_real_tool", Scope::Design, "", "{}", 1)
        .unwrap_err();
    assert!(err.to_string().contains("unknown ai tool"));
}

#[test]
fn ai_plan_rejects_malformed_context_json() {
    let (s, _g, project_path) = make_service_with_project();
    let err = s
        .ai_plan(
            &project_path,
            "style_assistant",
            Scope::Design,
            "",
            "not json {{{",
            1,
        )
        .unwrap_err();
    assert!(err.to_string().contains("context_json"), "got error: {err}");
}

#[test]
fn ai_accept_diff_removes_pending_diff() {
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
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
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let outcome = s.ai_reject_diff(&result.diff_id, None).unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.diff_id, result.diff_id);

    let st = s.ai_runtime_status().unwrap();
    assert!(st.pending_diff_ids.is_empty());
}

#[test]
fn ai_accept_diff_applies_commands_and_writes_audit() {
    // Phase 11 task 10 end-to-end: a style_assistant plan
    // produces 4 diff operations (2 furniture, 1 material_binding,
    // 1 lighting_preset). The converter applies the 2 furniture
    // and the 1 lighting_preset; the material_binding is skipped
    // because the diff engine doesn't emit `target_entity`.
    // After accept:
    //   * `project_graph_list` reports 2 furniture entities.
    //   * `outcome.command_ids` has 3 entries (2 furniture +
    //     1 lighting_preset; SetLighting produces no delta but
    //     still gets a journal entry).
    //   * `outcome.skipped` has 1 entry for the material binding.
    //   * The AI audit chain head advanced past genesis.
    //   * The forensic `ai_records.jsonl` file exists and
    //     contains the diff id.
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let pre_furniture = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list pre-accept");
    assert!(
        pre_furniture.is_empty(),
        "fresh project must have no furniture yet, got {pre_furniture:?}"
    );

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let outcome = s.ai_accept_diff(&result.diff_id).unwrap();

    assert!(outcome.ok, "accept must succeed");
    assert_eq!(outcome.op_count, 4, "style_assistant emits 4 ops");
    assert_eq!(
        outcome.applied_count, 3,
        "2 furniture + 1 lighting_preset apply; material_binding skipped (no target)"
    );
    assert_eq!(outcome.skipped.len(), 1, "material_binding must be skipped");
    assert!(
        outcome.skipped[0].reason.contains("material_binding"),
        "skip reason must mention material_binding, got: {}",
        outcome.skipped[0].reason
    );
    assert_eq!(
        outcome.command_ids.len(),
        3,
        "one command per applied op (incl. SetLighting which has empty delta but a journal entry)"
    );
    assert_ne!(
        outcome.audit_chain_head, "blake3:genesis",
        "audit chain must advance past genesis after accept"
    );

    let post_furniture = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list post-accept");
    assert_eq!(
        post_furniture.len(),
        2,
        "two furniture inserts must land in the graph, got {post_furniture:?}"
    );

    // Forensic record file must exist and contain the diff id +
    // tool name so a security reviewer can reconstruct the AI
    // proposal lineage even with the chained log alone.
    let records_path = std::path::Path::new(&project_path)
        .join("audit")
        .join("ai_audit_records.jsonl");
    let records = std::fs::read_to_string(&records_path)
        .expect("ai_audit_records.jsonl must exist after accept");
    assert!(
        records.contains("style_assistant"),
        "forensic record must name the tool, got: {records}"
    );
    assert!(
        records.contains("\"status\":\"accepted\""),
        "forensic record must capture the Accepted status, got: {records}"
    );
}

#[test]
fn ai_reject_diff_writes_reason_to_forensic_log() {
    // Phase 11 task 11: reject path captures the renderer-supplied
    // reason in the forensic companion file (NOT in the chained
    // log, which only carries hashes).
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();

    let outcome = s
        .ai_reject_diff(&result.diff_id, Some("doesn't match the brief"))
        .unwrap();
    assert!(outcome.ok);
    assert_eq!(outcome.op_count, 4, "rejecting must still report op count");
    assert_eq!(
        outcome.reason.as_deref(),
        Some("doesn't match the brief"),
        "reject outcome echoes the renderer-supplied reason"
    );
    assert_ne!(
        outcome.audit_chain_head, "blake3:genesis",
        "audit chain must advance past genesis after reject"
    );

    // Rejected diffs MUST NOT touch the project graph.
    let furniture = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list");
    assert!(
        furniture.is_empty(),
        "rejected diff must not mutate the graph, got {furniture:?}"
    );

    let records_path = std::path::Path::new(&project_path)
        .join("audit")
        .join("ai_audit_records.jsonl");
    let records = std::fs::read_to_string(&records_path)
        .expect("ai_audit_records.jsonl must exist after reject");
    assert!(
        records.contains("doesn't match the brief"),
        "rejection reason must reach the forensic log, got: {records}"
    );
    assert!(
        records.contains("\"status\":\"rejected\""),
        "forensic record must capture the Rejected status, got: {records}"
    );

    // The chained log (`ai_audit.jsonl`) must NOT contain the
    // reason text \u2014 chain integrity is via the hash, not the
    // payload.
    let chain_path = std::path::Path::new(&project_path)
        .join("audit")
        .join("ai_audit.jsonl");
    let chain = std::fs::read_to_string(&chain_path).expect("ai_audit.jsonl must exist");
    assert!(
        !chain.contains("doesn't match the brief"),
        "reason must NOT leak into the chained log, got: {chain}"
    );
}

#[test]
fn ai_accept_diff_rejects_unknown_id() {
    let (mut s, _g) = make_service();
    let err = s.ai_accept_diff("diff_does_not_exist").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

/// `BUG_0001 (round 2)` regression: if `ai_accept_diff` fails after
/// the initial peek (e.g. the project package can no longer be
/// opened because the on-disk files were moved/deleted), the pending
/// diff must survive in the registry so the renderer can retry once
/// the underlying problem is resolved. Prior to the peek/finalize
/// split, the diff was popped up-front and any downstream error
/// dropped it from the registry permanently.
#[test]
fn ai_accept_diff_failure_preserves_pending_diff_for_retry() {
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    // Sanity: the diff is registered before we corrupt the project.
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(
        st.pending_diff_ids,
        vec![result.diff_id.clone()],
        "diff must be pending after ai_plan"
    );

    // Force `ai_accept_diff_inner` to fail by removing the project
    // directory. `ProjectPackage::open_with_master_key_and_database`
    // will fail before any SQL is touched, so the inner method
    // returns an error without committing anything.
    std::fs::remove_dir_all(&project_path).expect("remove project dir");

    let err = s.ai_accept_diff(&result.diff_id).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("not found"),
        "the failure must come from the package open, not the diff registry: {msg}"
    );

    // The diff must STILL be in the registry so the renderer can
    // retry after the user restores the project.
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(
        st.pending_diff_ids,
        vec![result.diff_id.clone()],
        "BUG_0001 (round 2): pending diff must survive a failed accept"
    );
}

/// `BUG_0001 (round 2)` regression for the reject path: same
/// invariant as the accept-path test above. The audit append is the
/// only fallible step in reject, but any failure there must NOT
/// drop the pending diff.
#[test]
fn ai_reject_diff_failure_preserves_pending_diff_for_retry() {
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(
        st.pending_diff_ids,
        vec![result.diff_id.clone()],
        "diff must be pending after ai_plan"
    );

    // Remove the project directory so `ProjectPackage::open_with_master_key`
    // (called inside `ai_reject_diff_inner`) fails before any audit
    // entry is written.
    std::fs::remove_dir_all(&project_path).expect("remove project dir");

    let err = s
        .ai_reject_diff(&result.diff_id, Some("retry me"))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("not found"),
        "the failure must come from the package open, not the diff registry: {msg}"
    );

    let st = s.ai_runtime_status().unwrap();
    assert_eq!(
        st.pending_diff_ids,
        vec![result.diff_id.clone()],
        "BUG_0001 (round 2): pending diff must survive a failed reject"
    );
}

/// `BUG_0001 (round 3)` regression: if the post-commit AI audit
/// append fails *after* `execute_persistent_batch` has already
/// written the SQL transaction, the pending diff MUST be removed
/// from the registry. The earlier (round 2) structure left the
/// diff in the registry on any inner error — including audit
/// failure — which sounds safe but actually corrupts the project
/// on retry: `diff_to_commands` generates fresh `EntityId::new()`
/// UUIDs for every `Insert` op, so a second successful commit
/// would silently duplicate every inserted entity. This test
/// pre-creates the AI audit JSONL path as a *directory* so the
/// audit logger's `File::open` returns `EISDIR` AFTER the SQL
/// commit has already landed, then asserts:
///   1. `ai_accept_diff` returns Err (audit divergence surfaced).
///   2. The pending diff is no longer in the registry (finalize
///      ran between commit and audit).
///   3. The graph has the expected entities (commit was durable).
///   4. A retry returns the "diff not found" error rather than
///      double-applying.
#[test]
fn ai_accept_diff_failed_audit_after_commit_finalizes_to_block_retry_duplication() {
    let (mut s, _g, project_path) = make_service_with_project();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let st = s.ai_runtime_status().unwrap();
    assert_eq!(
        st.pending_diff_ids,
        vec![result.diff_id.clone()],
        "diff must be pending after ai_plan"
    );

    // Sabotage the AI audit path: replace the would-be chain file
    // with a *directory* of the same name. `AiAuditLogger::open`
    // calls `File::open` on this path (via `AuditLog::open`)
    // because it now `exists()` — and `File::open` on a directory
    // returns EISDIR. Critically, this leaves `<project>/audit/`
    // itself writable so `execute_persistent_batch` doesn't trip
    // over it first; only the audit-append step (phase 4) fails.
    let audit_dir = std::path::Path::new(&project_path).join("audit");
    std::fs::create_dir_all(&audit_dir).expect("ensure audit dir");
    let chain_as_dir = audit_dir.join("ai_audit.jsonl");
    std::fs::create_dir_all(&chain_as_dir).expect("create chain path as dir");

    let pre_furniture = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list pre-accept");
    assert!(
        pre_furniture.is_empty(),
        "fresh project must have no furniture yet"
    );

    let err = s.ai_accept_diff(&result.diff_id).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("not found"),
        "the failure must come from the audit append, not the diff registry: {msg}"
    );

    // Phase 3 finalize MUST have removed the diff from the
    // registry. If it hadn't, the renderer could retry and
    // `diff_to_commands` would generate fresh entity UUIDs,
    // double-applying every insert against the still-committed
    // graph.
    let st = s.ai_runtime_status().unwrap();
    assert!(
        st.pending_diff_ids.is_empty(),
        "BUG_0001 (round 3): pending diff must be finalized once SQL is committed, even if the post-commit audit append fails; got pending={:?}",
        st.pending_diff_ids
    );

    // Phase 2 SQL commit was durable: the furniture entities are
    // in the graph.
    let post_furniture = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list post-accept");
    assert_eq!(
        post_furniture.len(),
        2,
        "the 2 furniture inserts must be on disk even though audit append failed; got {post_furniture:?}"
    );

    // A retry now is structurally impossible (the diff is gone).
    // This is the property we want — no double-apply.
    let retry_err = s.ai_accept_diff(&result.diff_id).unwrap_err();
    assert!(
        retry_err.to_string().contains("not found"),
        "retry after failed audit must surface 'diff not found' rather than re-applying, got: {retry_err}"
    );

    // Sanity: the graph entity count didn't grow on retry.
    let after_retry = s
        .project_graph_list(&project_path, Some("furniture"))
        .expect("graph list post-retry");
    assert_eq!(
        after_retry.len(),
        2,
        "retry must not have duplicated entities; got {after_retry:?}"
    );
}

#[test]
fn ai_accept_diff_rejects_malformed_id() {
    let (mut s, _g) = make_service();
    // Missing the `diff_` prefix — DiffId::from_string rejects it.
    let err = s.ai_accept_diff("not-a-diff-id").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn ai_cancel_job_resets_runtime_to_idle() {
    let (mut s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let summary = s
        .project_create_from_template("interior.studio", "AI Endpoints Cancel")
        .expect("create project");
    let project_path = summary.path;
    // Drive the runtime to Ready via a successful plan.
    let _ = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
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
    let (mut s, _g) = make_service();
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
    wire_ai_state_to_mock(&mut s, port);

    let summary = s
        .project_create_from_template("interior.studio", "AI Endpoints Safety")
        .expect("create project");
    let project_path = summary.path;
    let err = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
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
    let (mut s, _g) = make_service();
    let listener = bind_loopback();
    let port = listener.local_addr().unwrap().port();
    let resp = canned_response(valid_style_assistant_response_bytes());
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut s, port);

    let summary = s
        .project_create_from_template("interior.studio", "AI Endpoints Json")
        .expect("create project");
    let project_path = summary.path;
    let result = s
        .ai_plan(&project_path, "style_assistant", Scope::Design, "", "{}", 5)
        .unwrap();
    let json = serde_json::to_string(&result).expect("AiPlanResult must serialise");
    assert!(!json.contains("inf"));
    assert!(!json.contains("NaN"));
}

/// Cold-spawn responsiveness contract.
///
/// The architectural fix at the heart of this PR is the split-`AiState`
/// design: lifecycle state (`runtime: RwLock`), spawn slot
/// (`handle_slot: Mutex`), and diff registry (`pending_diffs: Mutex`)
/// each have their own primitive. `AiState::snapshot()` — which
/// backs `BridgeService::ai_runtime_status` and therefore the
/// renderer's status pane — reads `runtime` and `pending_diffs` but
/// deliberately does NOT touch `handle_slot`.
///
/// This test pins that contract: even if some other thread is
/// holding `handle_slot` for an arbitrarily long time (the real
/// scenario being `ensure_ready` blocking on `/health` for up to 30
/// s during cold-spawn), `snapshot()` must still return in
/// microseconds with the published `Loading` state.
///
/// Implementation note: we hold `handle_slot` for 500 ms on a
/// background thread and assert the foreground `snapshot()` resolves
/// in under 100 ms. The 100 ms bound is generous enough to absorb
/// scheduler jitter on a loaded CI host while still being **5x
/// tighter** than the 500 ms hold — a regression that re-introduces
/// the outer `Mutex<AiState>` would block `snapshot()` for the full
/// 500 ms and fail this assertion by an order of magnitude.
#[test]
fn ai_runtime_status_returns_loading_instantly_during_cold_spawn() {
    let state = Arc::new(AiState::new(RuntimeConfig::default()));
    // Set up the world: state is `Loading` (just like the cold-spawn
    // path between `begin_load` and `mark_ready`/`mark_failed`).
    state.__test_begin_load();

    // Background thread holds `handle_slot` for 500 ms, mimicking
    // `ensure_ready` blocking on `/health`.
    let blocker = {
        let s = state.clone();
        thread::spawn(move || s.__test_hold_handle_slot_for(Duration::from_millis(500)))
    };

    // Give the blocker enough time to actually acquire the lock
    // before we measure. 50 ms is well above the cost of a thread
    // spawn + lock acquisition on any reasonable CI host.
    thread::sleep(Duration::from_millis(50));

    let start = Instant::now();
    let snap = state
        .snapshot()
        .expect("snapshot must not block on handle_slot");
    let elapsed = start.elapsed();

    assert_eq!(
        snap.state,
        RuntimeState::Loading,
        "snapshot must observe the published Loading state, not stale Idle",
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "snapshot took {elapsed:?}; expected < 100 ms. Did the outer Mutex<AiState> creep back in?",
    );

    blocker.join().expect("blocker thread");
}
