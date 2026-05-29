//! Phase 4 end-to-end user journey — Construction PM ("Site
//! renovation with BIM Lite and BOQ"). Driven entirely through the
//! `BridgeService` public API.
//!
//! Steps (PROPOSAL.md "User Journey C"):
//!
//!   1. Create project from the apartment template (any project
//!      will do — the journey's payload is the attached IFC, not
//!      the template).
//!   2. Import an IFC fixture (`bim_import_ifc`) — exercises the
//!      preview-summary surface the renderer uses on the import
//!      dialog.
//!   3. Attach the IFC to the project (`bim_attach_ifc`) — pushes
//!      the IFC's spatial hierarchy into the encrypted project
//!      package so subsequent classify / set_property calls see the
//!      attached entities.
//!   4. Run BIM validation (`bim_validate`).
//!   5. Classify entities (`bim_classify`) with the `uniformat-ii`
//!      scheme.
//!   6. Set a property on the seeded wall (`bim_set_property`).
//!   7. Generate room + door schedules (`bim_generate_schedule`,
//!      twice) → both XLSX outputs exist on disk.
//!   8. Export the BOQ XLSX — the schedule generated in step 7 is
//!      the BOQ surface; `deliver_build_pack` with `include_boq=true`
//!      wraps it into a contractor archive.
//!   9. Export the BIM Lite deliver pack
//!      (`deliver_build_pack` with `kind="bim"`).
//!
//! The fixture is the same `small_office.ifc` used by
//! `bim_readonly_ops.rs` / `bim_attach_fixture.rs` — a single source
//! of truth across the BIM test surface, so any change to it shows
//! up consistently across every test that depends on the IFC4 wire
//! shape.

use std::path::{Path, PathBuf};

use aec_bridge::{BridgeConfig, BridgeService, DeliverBuildPackParams, DeliverPackInventoryFlags};
use aec_command::commands::{wall, Command, CommandKind};
use aec_core::types::EntityId;

const FIXTURE_BYTES: &[u8] = include_bytes!("fixtures/small_office.ifc");

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
    let svc = BridgeService::new(cfg, [0x4Cu8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

fn write_ifc_fixture() -> (PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small_office.ifc");
    std::fs::write(&path, FIXTURE_BYTES).unwrap();
    (path, dir)
}

#[test]
fn phase4_construction_pm_bim_lite_and_boq_journey() {
    // ── Step 1: create project ──
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Phase 4 BIM Journey")
        .expect("create project");

    // ── Step 2: import IFC (preview / summary surface) ──
    let (ifc_path, _ifc_g) = write_ifc_fixture();
    let import = svc
        .bim_import_ifc(ifc_path.to_str().unwrap())
        .expect("bim_import_ifc");
    assert!(
        import.elements > 0 || import.spatial_nodes > 0,
        "fixture must report at least one element / spatial node on import (got elements={}, spatial_nodes={})",
        import.elements,
        import.spatial_nodes
    );
    assert!(
        ["IFC4", "IFC4X3"].contains(&import.schema.as_str()),
        "fixture schema should be IFC4 / IFC4X3, got `{}`",
        import.schema
    );

    // ── Step 3: attach IFC to the project ──
    let attach = svc
        .bim_attach_ifc(&summary.path, ifc_path.to_str().unwrap())
        .expect("bim_attach_ifc");
    assert!(
        attach.spatial_nodes_inserted > 0,
        "attach should insert at least the IfcProject root"
    );

    // ── Step 4: run BIM validation against the IFC fixture ──
    let validate = svc
        .bim_validate(ifc_path.to_str().unwrap())
        .expect("bim_validate");
    // The fixture is a valid IFC4 file — we don't insist on
    // `errors.is_empty()` because the validator may legitimately
    // flag missing classifications / property sets (a typical
    // "lite" IFC4 fixture does not ship every recommended pset).
    // We just confirm the validator produced a structured report
    // with the expected schema string.
    assert!(
        validate.schema.starts_with("IFC"),
        "validator should echo the file's schema, got `{}`",
        validate.schema
    );
    // Make sure all three finding buckets are addressable (never
    // panics-on-deref) — a regression that returned `None`
    // collections instead of empty `Vec`s would surface here.
    let _ = (
        validate.errors.len(),
        validate.warnings.len(),
        validate.infos.len(),
    );

    // ── Step 5: classify entities (uniformat-ii) ──
    // The IFC attach pushed spatial nodes into the project but
    // didn't add a wall entity (the fixture's body element is
    // classified by `bim_attach` differently than a Phase 2 wall).
    // Seed an explicit wall through `command_apply` so the
    // uniformat-ii classifier has a deterministic entity to
    // operate on — this is exactly what `seed_one_wall` does in
    // the in-process tests.
    let wall_id = EntityId::new();
    let cmd = Command::user(CommandKind::CreateWall(wall::CreateWall {
        entity_id: wall_id.clone(),
        start_mm: [0.0, 0.0],
        end_mm: [4_500.0, 0.0],
        height_mm: 2_700.0,
        thickness_mm: 100.0,
        material_id: None,
    }));
    svc.command_apply(&summary.path, cmd)
        .expect("seed wall through command_apply");

    let classify = svc
        .bim_classify(&summary.path, "uniformat-ii")
        .expect("bim_classify uniformat-ii");
    assert_eq!(classify.scheme, "uniformat-ii");
    assert!(
        classify.classified > 0,
        "uniformat-ii should classify the seeded wall"
    );
    // Walls map to Uniformat B2010 (exterior walls). The bim_classify
    // service-level tests on main pin this; mirror that assertion
    // at the journey level so a regression that, say, dropped the
    // wall→B2010 mapping shows up here too.
    let b2010_hits: usize = classify
        .details
        .iter()
        .filter(|d| d.code == "B2010")
        .count();
    assert!(
        b2010_hits >= 1,
        "the seeded wall should map to Uniformat B2010, got details: {:?}",
        classify.details
    );

    // ── Step 6: set a property on the seeded wall ──
    let prop = svc
        .bim_set_property(
            &summary.path,
            wall_id.as_str(),
            "Pset_WallCommon",
            "FireRating",
            "REI120",
        )
        .expect("bim_set_property");
    assert_eq!(prop.entity_id, wall_id.as_str());
    assert_eq!(prop.pset, "Pset_WallCommon");
    assert_eq!(prop.key, "FireRating");

    // ── Step 7: generate room + door schedules from the IFC ──
    let sched_out_dir = tempfile::tempdir().unwrap();
    let room_xlsx = sched_out_dir.path().join("rooms.xlsx");
    let door_xlsx = sched_out_dir.path().join("doors.xlsx");

    let room_sched = svc
        .bim_generate_schedule(
            ifc_path.to_str().unwrap(),
            "room",
            room_xlsx.to_str().unwrap(),
        )
        .expect("generate room schedule");
    assert_eq!(room_sched.kind, "room");
    assert!(room_xlsx.exists(), "room schedule xlsx should land on disk");
    assert!(
        room_sched.bytes_written > 0,
        "room schedule should write >0 bytes"
    );
    assert!(
        room_sched.columns >= 3,
        "room schedule should have at least 3 columns (mark, area, finish)"
    );

    let door_sched = svc
        .bim_generate_schedule(
            ifc_path.to_str().unwrap(),
            "door",
            door_xlsx.to_str().unwrap(),
        )
        .expect("generate door schedule");
    assert_eq!(door_sched.kind, "door");
    assert!(door_xlsx.exists(), "door schedule xlsx should land on disk");
    assert!(
        door_sched.bytes_written > 0,
        "door schedule should write >0 bytes"
    );

    // ── Step 8: export BOQ XLSX (via contractor deliver pack with
    //   include_boq=true). This is the "bill of quantities" surface
    //   the renderer ships under the BIM workflow. ──
    let boq_dir = tempfile::tempdir().unwrap();
    let boq_path = boq_dir.path().join("boq_pack.zip");
    let boq = svc
        .deliver_build_pack(DeliverBuildPackParams {
            out_path: boq_path.to_string_lossy().into_owned(),
            kind: "contractor".into(),
            project_name: "Phase 4 BIM Journey".into(),
            options: DeliverPackInventoryFlags {
                include_renders: false,
                include_sheets: false,
                include_ifc: true,
                include_boq: true,
                include_proposal: false,
            },
            project_path: Some(summary.path.clone()),
        })
        .expect("deliver contractor boq pack");
    assert!(boq_path.exists());
    assert!(
        boq.contents.iter().any(|c| std::path::Path::new(c)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))),
        "contractor pack with include_boq=true must contain an xlsx entry; got {:?}",
        boq.contents
    );
    assert!(
        boq.contents.iter().any(|c| c.contains("contractor")),
        "contractor pack must contain the contractor summary PDF; got {:?}",
        boq.contents
    );

    // ── Step 9: export BIM Lite deliver pack ──
    let bim_dir = tempfile::tempdir().unwrap();
    let bim_pack_path = bim_dir.path().join("bim_lite_pack.zip");
    let bim_pack = svc
        .deliver_build_pack(DeliverBuildPackParams {
            out_path: bim_pack_path.to_string_lossy().into_owned(),
            kind: "bim".into(),
            project_name: "Phase 4 BIM Journey".into(),
            options: DeliverPackInventoryFlags {
                include_renders: false,
                include_sheets: false,
                include_ifc: true,
                include_boq: true,
                include_proposal: false,
            },
            project_path: Some(summary.path.clone()),
        })
        .expect("deliver bim pack");
    assert!(bim_pack_path.exists());
    let bim_pack_size = std::fs::metadata(&bim_pack_path).unwrap().len();
    assert!(bim_pack_size > 1_000, "BIM pack should be > 1KB");
    assert!(
        bim_pack
            .contents
            .iter()
            .any(|c| c == "validation_report.pdf"),
        "BIM pack must contain the validation_report.pdf summary; got {:?}",
        bim_pack.contents
    );

    // Sanity: project still saveable after the full journey.
    svc.project_save(&summary.path)
        .expect("project_save after bim journey");
}

#[test]
fn phase4_journey_schedule_kinds_are_independently_addressable() {
    // Independent of the main journey: verify each of the 4
    // supported schedule kinds writes a distinct XLSX from the same
    // IFC file. Regression guard against a refactor that would
    // silently collapse two kinds onto the same writer.
    let (svc, _g) = boot_service();
    let (ifc_path, _ifc_g) = write_ifc_fixture();
    let out = tempfile::tempdir().unwrap();

    for kind in &["door", "window", "room", "material"] {
        let xlsx = out.path().join(format!("{kind}.xlsx"));
        let summary = svc
            .bim_generate_schedule(ifc_path.to_str().unwrap(), kind, xlsx.to_str().unwrap())
            .unwrap_or_else(|e| panic!("schedule {kind}: {e}"));
        assert_eq!(summary.kind, *kind);
        assert!(xlsx.exists(), "{kind} schedule must write a real xlsx");
        assert!(
            summary.bytes_written > 0,
            "{kind} schedule should be non-empty (xlsx headers alone exceed 0 bytes)"
        );
    }
}
