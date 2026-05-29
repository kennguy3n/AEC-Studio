//! Phase 14 Group F (Task 7) — Performance benchmarks pinned against
//! the customer-visible budgets from PROPOSAL.md and the Phase 6
//! exit criteria.
//!
//! Five benches, one per criterion:
//!
//! | bench                                    | budget         |
//! |------------------------------------------|----------------|
//! | `app_cold_start`                         | < 2.5 s        |
//! | `project_create_from_template_apartment` | < 1.0 s        |
//! | `dxf_import_10k_entities`                | < 1.5 s        |
//! | `ai_plan_mock_sidecar`                   | < 1.5 s        |
//! | `contractor_handoff_pack`                | < 60 s         |
//!
//! Run with:
//!
//! ```text
//! cargo bench -p aec_bridge --bench acceptance_criteria
//! ```
//!
//! The numbers Criterion reports are wall-clock measurements on the
//! host that runs the bench. CI machines are typically slower than a
//! developer workstation, so a paired test file
//! `tests/acceptance_criteria_targets.rs` runs each operation **once**
//! (outside Criterion) and asserts within 3× the customer budget. The
//! 3× headroom catches outright regressions (e.g. a 10× slowdown)
//! without flapping on noisy CI runners.
//!
//! ### Notes on shape
//!
//! * Each bench uses `iter_batched_ref` with a setup closure that
//!   builds a fresh `BridgeService` (or fresh project) so successive
//!   iterations don't accumulate state. The setup is *not* counted in
//!   the measured time — only the inner closure is.
//! * The `ai_plan` bench wires a mock TCP sidecar via
//!   `BridgeService::__test_install_ai_state`. The 1.5 s budget in
//!   PROPOSAL.md is for the real 1.7B-tier `llama-server`; the mock
//!   short-circuits the model so this bench is measuring the bridge's
//!   wire path (planner dispatch + diff build + pending diff
//!   registration), not LLM latency.
//! * The `contractor_pack` bench creates a project from the apartment
//!   template and runs the full `deliver_build_pack` end-to-end. The
//!   template provides enough graph content (walls, rooms, materials)
//!   that the resulting pack carries real IFC + schedules + cover PDF
//!   — not the empty-context fallback shape.

use std::hint::black_box;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use aec_ai::{RuntimeConfig, SidecarTransport};
use aec_bridge::{
    ai_state::AiState, BridgeConfig, BridgeService, DeliverBuildPackParams,
    DeliverPackInventoryFlags,
};
use aec_cad::dxf::{DxfDocument, DxfEntity, DxfLine, DxfWriter};
use aec_core::Scope;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

// ---------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------

/// Shipped templates directory, walked relative to the crate manifest.
/// The bench mirrors `tests/deliver_pack_with_project.rs` so the
/// apartment template fixture stays in one place.
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

/// Build a fresh `BridgeService` against a tempdir-rooted state /
/// projects / templates layout, pre-seeded with the apartment
/// template. Returns the service alongside the tempdir guard so the
/// caller can keep the on-disk state alive for the duration of the
/// measurement.
fn boot_service() -> (BridgeService, TempDir) {
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

/// Boot a service AND create an apartment project so DXF-import /
/// AI-plan / deliver-pack benches have a project to operate on.
fn boot_service_with_project() -> (BridgeService, TempDir, String) {
    let (mut svc, guard) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Acceptance Criteria Bench")
        .expect("create apartment project");
    let path = summary.path;
    (svc, guard, path)
}

/// Generate a 10 000-entity DXF file at `path`. Each entity is a
/// `LINE` (the simplest supported primitive) on layer `0` so the
/// reader's converter path lights up across the import. Returns the
/// path for convenience.
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

// ---------------------------------------------------------------
// Mock sidecar
// ---------------------------------------------------------------

/// Bind a fresh loopback port. Returns `(listener, port)`.
fn bind_loopback() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// HTTP wire-format response that the mock sidecar emits for every
/// incoming `/completion` request. The body is a valid
/// `style_assistant` plan (the simplest tool — three string-array
/// fields, no geometric structures) so the bridge can parse it and
/// the diff engine can register a pending diff. Built dynamically so
/// `Content-Length` stays in sync with the body bytes.
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

/// Spawn a mock sidecar that answers `count` `/completion` requests
/// with the same canned response, then exits. Criterion bumps the
/// sample count when the operation is fast, so `count` should be
/// generously over-provisioned — extras are silently dropped when the
/// listener goes out of scope.
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

/// Inject an `AiState` bound to a mock-sidecar TCP port. Mirrors the
/// `tests/ai_endpoints.rs::wire_ai_state_to_mock` helper.
fn wire_ai_state_to_mock(service: &mut BridgeService, port: u16) {
    let transport = SidecarTransport::new(port, Duration::from_secs(5));
    let state = AiState::__test_with_transport(RuntimeConfig::default(), transport);
    service.__test_install_ai_state(state);
}

// ---------------------------------------------------------------
// Benches
// ---------------------------------------------------------------

/// Bench 1 — App cold start.
///
/// `BridgeService::new` does the I/O-heavy work (recents store load,
/// engine-status cache init, key derivation), then `runtime_status`
/// runs the hardware profile + tier classifier. Together they
/// approximate what the renderer waits on between Electron app-ready
/// and the first frame of UI.
///
/// PROPOSAL.md budget: < 2.5 s on a reasonable workstation.
fn bench_app_cold_start(c: &mut Criterion) {
    c.bench_function("app_cold_start", |b| {
        b.iter_batched(
            // Pre-build the on-disk template tree so the closure
            // measures `BridgeService::new` + `runtime_status`, not
            // tempdir creation.
            || {
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
                (cfg, tmp)
            },
            |(cfg, _guard)| {
                let svc = BridgeService::new(cfg, [0x4Eu8; 32]).expect("boot BridgeService");
                let status = svc.runtime_status();
                // Tier classification is the public contract; the
                // black_box stops the optimiser from eliding the call.
                black_box(status.tier);
                black_box(status.cpu.logical_cores);
            },
            BatchSize::PerIteration,
        );
    });
}

/// Bench 2 — Project create from the `interior.apartment` template.
///
/// Exercises `project_create_from_template`: read the template JSON,
/// allocate a new `.aecstudio` package on disk, write the encrypted
/// SQLCipher DB, seed the audit log, register the project with the
/// recents store. Mirrors the renderer's "new project from template"
/// gesture.
///
/// PROPOSAL.md budget: < 1.0 s.
fn bench_project_create_from_template_apartment(c: &mut Criterion) {
    c.bench_function("project_create_from_template_apartment", |b| {
        b.iter_batched_ref(
            // Each iter needs a fresh `BridgeService` because the
            // recents store / projects dir would otherwise carry
            // state across samples.
            boot_service,
            |(svc, _guard)| {
                svc.project_create_from_template("interior.apartment", "Bench Project")
                    .expect("create apartment");
            },
            BatchSize::PerIteration,
        );
    });
}

/// Bench 3 — DXF import of a programmatically generated 10 000-entity
/// document. Mirrors PROPOSAL.md's Journey D budget (drafter imports
/// a real DXF and gets a populated draft scope in under 1.5 s).
///
/// PROPOSAL.md budget: < 1.5 s.
fn bench_dxf_import_10k_entities(c: &mut Criterion) {
    // Generate the DXF once; it's the input to every iteration, not
    // part of the measured work.
    let dxf_dir = tempfile::tempdir().unwrap();
    let dxf_path = dxf_dir.path().join("entities_10k.dxf");
    write_10k_entity_dxf(&dxf_path);
    let dxf_path_str = dxf_path.to_string_lossy().into_owned();
    let dxf_path_str = Arc::new(dxf_path_str);

    c.bench_function("dxf_import_10k_entities", |b| {
        let dxf = Arc::clone(&dxf_path_str);
        b.iter_batched_ref(
            // Fresh project per iter — `draft_import_dxf` writes
            // entities to the project graph, so reusing one project
            // across iterations would have iteration N pay for the
            // accumulated commands of iterations 0..N-1.
            boot_service_with_project,
            |(svc, _guard, project_path)| {
                svc.draft_import_dxf(project_path, &dxf)
                    .expect("import dxf");
            },
            BatchSize::PerIteration,
        );
    });
}

/// Bench 4 — `ai_plan` round-trip through a mock sidecar.
///
/// The mock short-circuits the LLM so the measurement is the bridge's
/// wire path (planner dispatch + safety validator + diff engine +
/// pending-diff registration), not the model. PROPOSAL.md's 1.5 s
/// budget targets the 1.7B-tier `llama-server`; the mock path should
/// be well inside that.
///
/// PROPOSAL.md budget: < 1.5 s (mock path is expected to be ≪).
fn bench_ai_plan_mock_sidecar(c: &mut Criterion) {
    c.bench_function("ai_plan_mock_sidecar", |b| {
        b.iter_batched_ref(
            || {
                // Boot fresh per-iter so the pending-diff map stays
                // small and the mock sidecar gets a clean port.
                let (mut svc, guard, project_path) = boot_service_with_project();
                let (listener, port) = bind_loopback();
                let resp = canned_style_assistant_response();
                // One request per iter, so count = 1 is enough.
                let join = spawn_mock_sidecar(listener, resp, 1);
                wire_ai_state_to_mock(&mut svc, port);
                (svc, guard, project_path, Some(join))
            },
            |(svc, _guard, project_path, _join)| {
                svc.ai_plan(
                    project_path,
                    "style_assistant",
                    Scope::Design,
                    "a warm evening",
                    "{}",
                    5,
                )
                .expect("ai_plan");
            },
            BatchSize::PerIteration,
        );
    });
}

/// Bench 5 — Contractor handoff pack export end-to-end through the
/// bridge.
///
/// Exercises `deliver_build_pack(kind="contractor")` against a real
/// apartment-template project. Includes the SQLCipher open, graph
/// load, material + BOQ schedule aggregation, IFC serialisation,
/// floor-plan SVG render, and ZIP container build.
///
/// PROPOSAL.md budget: < 60 s.
fn bench_contractor_handoff_pack(c: &mut Criterion) {
    c.bench_function("contractor_handoff_pack", |b| {
        b.iter_batched_ref(
            boot_service_with_project,
            |(svc, _guard, project_path)| {
                let out_dir = tempfile::tempdir().unwrap();
                let zip_path = out_dir.path().join("contractor.zip");
                let res = svc
                    .deliver_build_pack(DeliverBuildPackParams {
                        out_path: zip_path.to_string_lossy().into_owned(),
                        kind: "contractor".into(),
                        project_name: "Bench Project".into(),
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
                // Keep tempdir alive past the assert so the archive
                // isn't dropped before the bench iteration ends.
                drop(out_dir);
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group! {
    name = benches;
    // Fewer samples than Criterion's default (100) — each iteration
    // creates a fresh on-disk project and a fresh `BridgeService`, so
    // wall-clock per sample is dominated by setup. 10 samples is
    // enough to surface a 2-3× regression without making the bench
    // unbearable to run interactively.
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(30))
        .warm_up_time(Duration::from_secs(3));
    targets =
        bench_app_cold_start,
        bench_project_create_from_template_apartment,
        bench_dxf_import_10k_entities,
        bench_ai_plan_mock_sidecar,
        bench_contractor_handoff_pack
}
criterion_main!(benches);
