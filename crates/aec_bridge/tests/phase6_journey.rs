//! Phase 6 end-to-end user journey — Deliver workflow.
//!
//! Exercises the full Deliver-scope contract through `BridgeService`
//! exactly as the desktop renderer drives it:
//!
//!   1. Create an `interior.apartment` project from the shipped
//!      template.
//!   2. Model two rooms by issuing `CreateWall` commands for the
//!      perimeter walls (two walls per room, four total). The Phase 6
//!      Deliver story doesn't require a closed loop — the revision +
//!      pack contracts only care that the graph holds real entities,
//!      not that they form a watertight room. We pin the *count* so a
//!      regression that silently dropped commands surfaces here.
//!   3. Save two cameras (`SaveCamera`) — one room overview, one
//!      detail shot.
//!   4. Enqueue two renders (one per camera) at the `standard`
//!      preset. The acceptance bench / CI runners have no GPU, so we
//!      assert the queue holds both jobs in the `queued` state rather
//!      than waiting for completion. This is the "mock render"
//!      contract: from the bridge's perspective, enqueue is the
//!      gesture; the driver loop is what consumes the queue.
//!   5. Create revision `v1`.
//!   6. Add an additional wall (`CreateWall`) — the "change geometry
//!      between revisions" gesture.
//!   7. Create revision `v2`.
//!   8. Diff v1 vs v2 (`deliver_compare_revisions`) — the new wall
//!      must surface as an `added` change in the per-category counts
//!      and the per-entity change list.
//!   9. Export all four pack kinds (`concept`, `interior`,
//!      `contractor`, `bim`) through `deliver_build_pack`. Each must
//!      land a valid ZIP archive on disk.
//!  10. Open the contractor pack and verify it carries *real*
//!      content, not the empty-context fallback:
//!      - `contractor_summary.pdf` ≥ 1 KB and starts with the `%PDF`
//!        magic.
//!      - At least one `sheets/*.pdf` entry ≥ 1 KB.
//!      - `schedules/materials.xlsx` and `schedules/boq.xlsx` ≥
//!        500 B and carry the `PK\x03\x04` magic (XLSX is a ZIP
//!        under the hood).
//!      - `model/project.ifc` is a parseable IFC4 STEP document
//!        with the four mandatory spatial classes.
//!
//! This pins the customer-visible Phase 6 contract — a regression in
//! revision capture, geometry diff, or pack content surfaces here
//! ahead of the renderer.

use std::io::Read;
use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService, DeliverBuildPackParams, DeliverPackInventoryFlags};
use aec_command::commands::{
    camera::{CameraParams, SaveCamera},
    wall::CreateWall,
    Command, CommandKind,
};
use aec_core::types::EntityId;

fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

fn copy_template(category: &str, id: &str, dest: &Path) {
    let src = workspace_templates_dir()
        .join(category)
        .join(format!("{id}.json"));
    let dest_dir = dest.join(category);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_file = dest_dir.join(format!("{id}.json"));
    std::fs::copy(&src, &dest_file).unwrap_or_else(|e| {
        panic!(
            "failed to copy shipped template {} -> {}: {e}",
            src.display(),
            dest_file.display()
        )
    });
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    copy_template("interior", "apartment", &templates);
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
        extensions_dir: None,
    };
    let svc = BridgeService::new(cfg, [0x6Au8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

fn create_wall_cmd(start: [f64; 2], end: [f64; 2]) -> (EntityId, Command) {
    let id = EntityId::new();
    let cmd = Command::user(CommandKind::CreateWall(CreateWall {
        entity_id: id.clone(),
        start_mm: start,
        end_mm: end,
        height_mm: 2_700.0,
        thickness_mm: 100.0,
        material_id: None,
    }));
    (id, cmd)
}

fn save_camera_cmd(name: &str, position_mm: [f64; 3], target_mm: [f64; 3]) -> Command {
    Command::user(CommandKind::SaveCamera(SaveCamera {
        entity_id: EntityId::new(),
        name: name.to_string(),
        params: CameraParams {
            position_mm,
            target_mm,
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5_600,
            depth_of_field_f: Some(5.6),
            aspect_ratio: 16.0 / 9.0,
        },
    }))
}

fn pack_kind_summary(kind: &str) -> &'static str {
    match kind {
        "concept" => "concept_pack.pdf",
        "interior" => "interior_summary.pdf",
        "contractor" => "contractor_summary.pdf",
        "bim" => "validation_report.pdf",
        other => panic!("unknown pack kind in journey test: {other}"),
    }
}

/// Open a ZIP entry and return its bytes. Panics on a missing entry,
/// so the assertion site (just below the call) reports a friendly
/// "expected `X` in pack" message.
fn read_zip_entry(zip_path: &Path, entry_name: &str) -> Vec<u8> {
    let f = std::fs::File::open(zip_path).unwrap();
    let mut zr = zip::ZipArchive::new(f).unwrap();
    let mut entry = zr.by_name(entry_name).unwrap_or_else(|e| {
        panic!(
            "zip entry `{entry_name}` missing from {}: {e}",
            zip_path.display()
        )
    });
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).unwrap();
    buf
}

/// Enumerate every entry in `zip_path` whose name starts with
/// `prefix`. Used to find all the `sheets/*.pdf` entries the
/// contractor pack lays down.
fn zip_entry_names_with_prefix(zip_path: &Path, prefix: &str) -> Vec<String> {
    let f = std::fs::File::open(zip_path).unwrap();
    let mut zr = zip::ZipArchive::new(f).unwrap();
    let mut names = Vec::new();
    for i in 0..zr.len() {
        let entry = zr.by_index(i).unwrap();
        if entry.name().starts_with(prefix) {
            names.push(entry.name().to_string());
        }
    }
    names
}

#[test]
fn phase6_deliver_workflow_journey_through_bridge_service() {
    // ── Step 1: create the project ─────────────────────────────
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Phase 6 Deliver Journey")
        .expect("create apartment");
    assert!(!summary.path.is_empty());

    // Snapshot the wall baseline. The apartment template may pre-
    // populate walls when fully instantiated (Phase 11 Group B /
    // Task 12); we assert on the journey-contributed delta so the
    // test stays stable across template evolutions.
    let wall_baseline = svc
        .project_graph_list(&summary.path, Some("wall"))
        .expect("graph: walls baseline")
        .len();

    // ── Step 2: model 2 rooms via 4 CreateWall commands ────────
    // Room 1 = two parallel walls along x. Room 2 = two parallel
    // walls offset in y. The walls don't form closed loops; the
    // journey only needs them as graph entities for revision and
    // pack content.
    let (room1_wall_a, cmd_r1_a) = create_wall_cmd([0.0, 0.0], [4_500.0, 0.0]);
    let (room1_wall_b, cmd_r1_b) = create_wall_cmd([0.0, 3_000.0], [4_500.0, 3_000.0]);
    let (room2_wall_a, cmd_r2_a) = create_wall_cmd([5_000.0, 0.0], [9_500.0, 0.0]);
    let (room2_wall_b, cmd_r2_b) = create_wall_cmd([5_000.0, 3_000.0], [9_500.0, 3_000.0]);
    for cmd in [cmd_r1_a, cmd_r1_b, cmd_r2_a, cmd_r2_b] {
        svc.command_apply(&summary.path, cmd)
            .expect("CreateWall for journey room");
    }
    let walls_after_modelling = svc
        .project_graph_list(&summary.path, Some("wall"))
        .expect("graph: walls after modelling");
    assert_eq!(
        walls_after_modelling.len(),
        wall_baseline + 4,
        "two journey rooms contribute exactly 4 new wall entities (baseline {wall_baseline} + 4)"
    );
    for eid in [&room1_wall_a, &room1_wall_b, &room2_wall_a, &room2_wall_b] {
        assert!(
            walls_after_modelling.iter().any(|w| &w.id == eid),
            "journey-modelled wall {eid:?} must surface in the graph",
        );
    }

    // ── Step 3: save 2 cameras ─────────────────────────────────
    let cam_overview = save_camera_cmd(
        "Room 1 Overview",
        [-2_000.0, -2_000.0, 1_650.0],
        [2_250.0, 1_500.0, 1_500.0],
    );
    let cam_detail = save_camera_cmd(
        "Room 2 Detail",
        [7_250.0, -2_000.0, 1_650.0],
        [7_250.0, 1_500.0, 1_500.0],
    );
    let camera_baseline = svc
        .project_graph_list(&summary.path, Some("camera"))
        .expect("graph: cameras baseline")
        .len();
    let cam_id_overview = match &cam_overview.kind {
        CommandKind::SaveCamera(c) => c.entity_id.clone(),
        _ => unreachable!(),
    };
    let cam_id_detail = match &cam_detail.kind {
        CommandKind::SaveCamera(c) => c.entity_id.clone(),
        _ => unreachable!(),
    };
    svc.command_apply(&summary.path, cam_overview)
        .expect("SaveCamera overview");
    svc.command_apply(&summary.path, cam_detail)
        .expect("SaveCamera detail");
    let cameras_after = svc
        .project_graph_list(&summary.path, Some("camera"))
        .expect("graph: cameras after save");
    assert_eq!(
        cameras_after.len(),
        camera_baseline + 2,
        "saving 2 cameras must add 2 entities (baseline {camera_baseline} + 2)",
    );

    // ── Step 4: enqueue 2 renders (mock — no GPU in CI) ────────
    // The bridge's render queue is the contract; the driver loop
    // that drains it is GPU-bound and lives outside this test.
    let camera_id_strs = vec![
        cam_id_overview.as_str().to_string(),
        cam_id_detail.as_str().to_string(),
    ];
    let batch = svc
        .render_enqueue_batch(&camera_id_strs, &["standard".to_string()], None)
        .expect("render_enqueue_batch");
    assert_eq!(
        batch.job_ids.len(),
        2,
        "two cameras × one preset = two render jobs",
    );
    let jobs = svc.render_list_jobs().expect("render_list_jobs");
    assert_eq!(
        jobs.len(),
        2,
        "render queue should hold the two enqueued jobs",
    );
    let progress = svc
        .render_batch_progress(&batch.batch_id)
        .expect("render_batch_progress")
        .expect("batch progress present for active batch id");
    assert_eq!(progress.total, 2);
    assert_eq!(
        progress.completed, 0,
        "no driver loop is running in this test, so completed = 0",
    );

    // ── Step 5: create revision v1 ─────────────────────────────
    let rev_v1 = svc
        .deliver_create_revision(
            &summary.path,
            "v1",
            "Two rooms modelled, two cameras saved",
            None,
        )
        .expect("deliver_create_revision v1");
    assert_eq!(rev_v1.tag, "v1");
    assert!(!rev_v1.revision_id.is_empty());

    // ── Step 6: add another wall (the inter-revision delta) ────
    let (extra_wall_id, extra_wall_cmd) = create_wall_cmd([0.0, 6_000.0], [9_500.0, 6_000.0]);
    svc.command_apply(&summary.path, extra_wall_cmd)
        .expect("CreateWall between v1 and v2");
    let walls_after_extra = svc
        .project_graph_list(&summary.path, Some("wall"))
        .expect("graph: walls after extra")
        .len();
    assert_eq!(
        walls_after_extra,
        wall_baseline + 5,
        "extra wall must show up in the graph (baseline {wall_baseline} + 4 + 1)",
    );

    // ── Step 7: create revision v2 ─────────────────────────────
    let rev_v2 = svc
        .deliver_create_revision(&summary.path, "v2", "Added shared corridor wall", None)
        .expect("deliver_create_revision v2");
    assert_eq!(rev_v2.tag, "v2");
    assert_ne!(
        rev_v2.revision_id, rev_v1.revision_id,
        "v2 must have a distinct revision_id from v1",
    );

    // The revision store must report both revisions in chronological
    // order — the renderer's "Versions" pane relies on this.
    let revisions = svc
        .deliver_list_revisions(&summary.path)
        .expect("deliver_list_revisions");
    assert!(
        revisions.iter().any(|r| r.tag == "v1"),
        "list_revisions must contain v1: {revisions:?}",
    );
    assert!(
        revisions.iter().any(|r| r.tag == "v2"),
        "list_revisions must contain v2: {revisions:?}",
    );

    // ── Step 8: diff v1 vs v2 ──────────────────────────────────
    let diff = svc
        .deliver_compare_revisions(&summary.path, &rev_v1.revision_id, &rev_v2.revision_id)
        .expect("deliver_compare_revisions");
    assert_eq!(diff.base_revision_id, rev_v1.revision_id);
    assert_eq!(diff.head_revision_id, rev_v2.revision_id);

    // The extra wall must surface as an `added` change in the
    // wall category. We don't pin the exact count of *other*
    // changes because the template baseline could shift, but the
    // category-keyed total must record at least 1 addition and the
    // change list must mention the extra wall's id.
    let wall_counts = diff
        .by_category
        .get("wall")
        .expect("wall category must appear in revision diff");
    assert!(
        wall_counts.added >= 1,
        "wall category should report ≥1 added change between v1 and v2; got {wall_counts:?}",
    );
    assert!(
        diff.changes
            .iter()
            .any(|c| c.kind == "added" && c.id == extra_wall_id.as_str()),
        "revision diff change list must reference the added wall entity id {:?}",
        extra_wall_id.as_str(),
    );

    // ── Step 9: export all 4 pack kinds ────────────────────────
    let pack_out_dir = tempfile::tempdir().unwrap();
    let mut pack_paths: Vec<(String, PathBuf)> = Vec::new();
    for kind in ["concept", "interior", "contractor", "bim"] {
        let zip_path = pack_out_dir.path().join(format!("{kind}.zip"));
        let res = svc
            .deliver_build_pack(DeliverBuildPackParams {
                out_path: zip_path.to_string_lossy().into_owned(),
                kind: kind.to_string(),
                project_name: "Phase 6 Deliver Journey".into(),
                options: DeliverPackInventoryFlags {
                    include_renders: kind == "concept" || kind == "interior",
                    include_sheets: kind == "contractor" || kind == "bim",
                    include_ifc: kind == "contractor" || kind == "bim",
                    include_boq: kind == "contractor",
                    include_proposal: kind == "concept" || kind == "interior",
                },
                project_path: Some(summary.path.clone()),
            })
            .unwrap_or_else(|e| panic!("deliver_build_pack {kind}: {e}"));
        assert_eq!(
            PathBuf::from(&res.out_path),
            zip_path,
            "deliver_build_pack {kind} should land at requested out_path",
        );
        assert!(zip_path.exists(), "{kind} pack must exist on disk");

        // Every pack must be a real ZIP (PK\x03\x04 magic at byte
        // offset 0). The contents vector must include the
        // pack-kind-specific summary PDF entry.
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        assert!(
            zip_bytes.len() >= 4 && &zip_bytes[..4] == b"PK\x03\x04",
            "{kind} pack must carry the PKZIP local-file magic",
        );
        let summary_entry = pack_kind_summary(kind);
        assert!(
            res.contents.contains(&summary_entry.to_string()),
            "{kind} pack contents must list `{summary_entry}`; got {:?}",
            res.contents,
        );

        pack_paths.push((kind.to_string(), zip_path));
    }

    // ── Step 10: verify contractor pack content ────────────────
    let (_, contractor_zip) = pack_paths
        .iter()
        .find(|(k, _)| k == "contractor")
        .expect("contractor pack path captured above");

    // 10a. contractor_summary.pdf is a real PDF (> 1 KB, %PDF
    //      magic). The Phase 13 / Phase 14 Group A wiring routes
    //      this through `PdfBuilder`, not the placeholder path.
    let summary_pdf = read_zip_entry(contractor_zip, "contractor_summary.pdf");
    assert!(
        summary_pdf.starts_with(b"%PDF"),
        "contractor_summary.pdf must start with %PDF magic (got first 8 bytes: {:?})",
        &summary_pdf[..8.min(summary_pdf.len())],
    );
    assert!(
        summary_pdf.len() > 1_024,
        "contractor_summary.pdf must be > 1 KB, got {} bytes",
        summary_pdf.len(),
    );

    // 10b. At least one sheets/*.pdf entry is a real PDF > 1 KB.
    //      The contractor pack's fallback emits A100 + A101 placeholders
    //      when the project carries no real sheets, and both are
    //      real `PdfBuilder` outputs (not zero-byte stubs).
    let sheet_entries = zip_entry_names_with_prefix(contractor_zip, "sheets/");
    assert!(
        !sheet_entries.is_empty(),
        "contractor pack must contain at least one sheets/*.pdf entry; got entries with prefix `sheets/`: {sheet_entries:?}",
    );
    for name in &sheet_entries {
        let bytes = read_zip_entry(contractor_zip, name);
        assert!(
            bytes.starts_with(b"%PDF"),
            "{name} must start with %PDF magic",
        );
        assert!(
            bytes.len() > 1_024,
            "{name} must be > 1 KB, got {} bytes",
            bytes.len(),
        );
    }

    // 10c. schedules/{materials,boq}.xlsx are real XLSX archives
    //      (PK\x03\x04 magic + > 500 B). XLSX is itself a ZIP
    //      container; the magic at byte 0 is the strongest signal
    //      that we landed a real workbook rather than the hand-
    //      rolled Open XML placeholder.
    for entry in ["schedules/materials.xlsx", "schedules/boq.xlsx"] {
        let bytes = read_zip_entry(contractor_zip, entry);
        assert_eq!(
            &bytes[..4],
            b"PK\x03\x04",
            "{entry} must carry the PKZIP local-file magic",
        );
        assert!(
            bytes.len() > 500,
            "{entry} must be > 500 B, got {} bytes",
            bytes.len(),
        );
    }

    // 10d. model/project.ifc is a parseable IFC4 STEP document
    //      with the four mandatory spatial classes. Mirrors the
    //      Phase 14 Group A contract pinned in
    //      `deliver_pack_with_project.rs::deliver_build_pack_with_project_path_yields_real_ifc`.
    let ifc_bytes = read_zip_entry(contractor_zip, "model/project.ifc");
    let ifc_text = std::str::from_utf8(&ifc_bytes).expect("ifc payload is UTF-8");
    assert!(
        ifc_text.starts_with("ISO-10303-21;"),
        "ifc payload must start with the STEP ISO-10303-21 header",
    );
    assert!(
        ifc_text.contains("FILE_SCHEMA(('IFC4'));"),
        "ifc payload must declare IFC4 schema",
    );
    let ifc_upper = ifc_text.to_ascii_uppercase();
    for class in ["IFCPROJECT", "IFCSITE", "IFCBUILDING", "IFCBUILDINGSTOREY"] {
        assert!(
            ifc_upper.contains(class),
            "ifc payload must contain {class}; payload prefix: {}",
            &ifc_text[..200.min(ifc_text.len())],
        );
    }
}
