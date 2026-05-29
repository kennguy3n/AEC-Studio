//! Phase 14 Group F (Task 7) — CI-gate companion to the
//! `benches/acceptance_criteria.rs` Criterion suite.
//!
//! The Criterion bench runs many iterations and reports a precise
//! wall-clock distribution; that's useful for catching small
//! regressions on a developer workstation, but it's too noisy for CI:
//! shared GitHub Actions runners can be 5–10× slower than a
//! workstation, and the bench's per-iter setup (boot a fresh
//! `BridgeService`, instantiate an apartment project) dominates
//! measurement on those machines.
//!
//! This test file runs each of the five PROPOSAL.md customer-visible
//! operations **once** with a wall-clock timer and asserts the result
//! is within **3×** the customer budget. The 3× headroom is sized to
//! catch outright regressions (a 10× slowdown is unambiguous) while
//! tolerating CI machine noise.
//!
//! | operation                                 | budget   | gate (3×) |
//! |-------------------------------------------|----------|-----------|
//! | `app_cold_start`                          | < 2.5 s  | < 7.5 s   |
//! | `project_create_from_template_apartment`  | < 1.0 s  | < 3.0 s   |
//! | `dxf_import_10k_entities`                 | < 1.5 s  | < 4.5 s   |
//! | `ai_plan_mock_sidecar`                    | < 1.5 s  | < 4.5 s   |
//! | `contractor_handoff_pack`                 | < 60 s   | < 180 s   |
//!
//! Mocks: only the AI sidecar is mocked, and only because the real
//! `llama-server` binary isn't installable in CI. Every other test
//! exercises the real production code path end-to-end.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use aec_ai::{RuntimeConfig, SidecarTransport};
use aec_bridge::{
    ai_state::AiState, BridgeConfig, BridgeService, DeliverBuildPackParams,
    DeliverPackInventoryFlags,
};
use aec_cad::dxf::{DxfDocument, DxfEntity, DxfLine, DxfWriter};
use aec_core::Scope;

// ---------------------------------------------------------------
// Fixture helpers — mirror benches/acceptance_criteria.rs so the
// two files stay in lockstep when the API surface evolves.
// ---------------------------------------------------------------

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_apartment_template(dest: &Path) {
    let src = workspace_templates_dir()
        .join("interior")
        .join("apartment.json");
    let dest_dir = dest.join("interior");
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::copy(&src, dest_dir.join("apartment.json")).expect("copy apartment template");
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_apartment_template(&templates);
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0x4Eu8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

fn boot_service_with_project() -> (BridgeService, tempfile::TempDir, String) {
    let (mut svc, guard) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Acceptance Criteria Gate")
        .expect("create apartment");
    (svc, guard, summary.path)
}

fn write_10k_entity_dxf(path: &Path) {
    let mut doc = DxfDocument::new();
    for i in 0..10_000u32 {
        let f = i as f64;
        let start = [(f * 0.1).cos() * 1000.0, (f * 0.1).sin() * 1000.0, 0.0];
        let end = [
            ((f + 0.5) * 0.1).cos() * 1000.0,
            ((f + 0.5) * 0.1).sin() * 1000.0,
            0.0,
        ];
        doc.push(DxfEntity::Line(DxfLine {
            layer: "0".into(),
            start,
            end,
        }));
    }
    let mut f = std::fs::File::create(path).expect("create dxf");
    DxfWriter::write(&doc, &mut f).expect("write dxf");
}

fn bind_loopback() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

fn canned_style_assistant_response() -> &'static [u8] {
    let body = br#"{"content":"{\"furniture_ids\":[\"a\",\"b\"],\"material_ids\":[\"c\"],\"lighting_preset_id\":\"warm_evening\"}","stop":true,"tokens_predicted":42}"#;
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    Box::leak(bytes.into_boxed_slice())
}

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
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let state = AiState::__test_with_transport(RuntimeConfig::default(), transport);
    service.__test_install_ai_state(state);
}

/// Run `op` once and assert it completes within `budget × 3`. The
/// elapsed time is printed to stderr so CI can record the actual
/// numbers in build logs — useful for tracking the trend over time
/// even when the assertion doesn't fire.
fn assert_within_budget<F: FnOnce()>(label: &'static str, budget: Duration, op: F) {
    let gate = budget * 3;
    let started = Instant::now();
    op();
    let elapsed = started.elapsed();
    eprintln!(
        "[acceptance] {label}: {:.3}s (budget {:.3}s, gate {:.3}s)",
        elapsed.as_secs_f64(),
        budget.as_secs_f64(),
        gate.as_secs_f64(),
    );
    assert!(
        elapsed < gate,
        "{label} took {:.3}s, which exceeds 3× the {:.3}s PROPOSAL.md budget ({:.3}s gate). \
         This is either an outright regression in the bridge or the operation has acquired \
         unbounded work — investigate before relaxing the gate.",
        elapsed.as_secs_f64(),
        budget.as_secs_f64(),
        gate.as_secs_f64(),
    );
}

// ---------------------------------------------------------------
// CI gates
// ---------------------------------------------------------------

#[test]
fn app_cold_start_within_three_x_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_apartment_template(&templates);
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };

    assert_within_budget("app_cold_start", Duration::from_millis(2_500), || {
        let svc = BridgeService::new(cfg, [0x4Eu8; 32]).expect("boot BridgeService");
        // First `runtime_status` is part of the cold-start
        // contract: that's what the renderer hits on launch to
        // populate the hardware-tier banner.
        let _ = svc.runtime_status();
    });
}

#[test]
fn project_create_from_template_apartment_within_three_x_budget() {
    let (mut svc, _g) = boot_service();
    assert_within_budget(
        "project_create_from_template_apartment",
        Duration::from_secs(1),
        || {
            svc.project_create_from_template("interior.apartment", "Gate Project")
                .expect("create apartment");
        },
    );
}

#[test]
fn dxf_import_10k_entities_within_three_x_budget() {
    let dxf_dir = tempfile::tempdir().unwrap();
    let dxf_path = dxf_dir.path().join("entities_10k.dxf");
    write_10k_entity_dxf(&dxf_path);
    let dxf_path_str = dxf_path.to_string_lossy().into_owned();

    let (mut svc, _g, project_path) = boot_service_with_project();
    assert_within_budget(
        "dxf_import_10k_entities",
        Duration::from_millis(1_500),
        || {
            svc.draft_import_dxf(&project_path, &dxf_path_str)
                .expect("import dxf");
        },
    );
}

#[test]
fn ai_plan_mock_sidecar_within_three_x_budget() {
    let (mut svc, _g, project_path) = boot_service_with_project();
    let (listener, port) = bind_loopback();
    let resp = canned_style_assistant_response();
    let _join = spawn_mock_sidecar(listener, resp, 1);
    wire_ai_state_to_mock(&mut svc, port);

    assert_within_budget("ai_plan_mock_sidecar", Duration::from_millis(1_500), || {
        svc.ai_plan(
            &project_path,
            "style_assistant",
            Scope::Design,
            "a warm evening",
            "{}",
            5,
        )
        .expect("ai_plan");
    });
}

#[test]
fn contractor_handoff_pack_within_three_x_budget() {
    let (svc, _g, project_path) = boot_service_with_project();
    let out_dir = tempfile::tempdir().unwrap();
    let zip_path = out_dir.path().join("contractor.zip");

    assert_within_budget("contractor_handoff_pack", Duration::from_secs(60), || {
        let res = svc
            .deliver_build_pack(DeliverBuildPackParams {
                out_path: zip_path.to_string_lossy().into_owned(),
                kind: "contractor".into(),
                project_name: "Gate Project".into(),
                options: DeliverPackInventoryFlags {
                    include_renders: false,
                    include_sheets: false,
                    include_ifc: true,
                    include_boq: true,
                    include_proposal: false,
                },
                project_path: Some(project_path.clone()),
            })
            .expect("deliver pack");
        assert!(!res.contents.is_empty());
    });
}
