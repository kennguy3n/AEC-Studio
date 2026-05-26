//! Bridge-layer end-to-end test for the DWG import / export wiring
//! added in PR-AD (Phase 11 Group C Task 17).
//!
//! The bridge service's `draft_export_dwg` / `draft_import_dwg`
//! methods are the entry-points that the renderer's
//! `bridge.draftImportDwg` / `bridge.draftExportDwg` IPC handlers
//! route through. The unit tests inside `service.rs` exercise the
//! method bodies in isolation; this test drives them in the same
//! sequence the renderer does:
//!
//! 1. Boot a fresh `BridgeService` against a temp `state_dir` /
//!    `projects_dir` / `templates_dir`.
//! 2. `project_create_from_template` an empty project.
//! 3. Seed two primitives (a Line and a Circle) via `command_apply`
//!    so the project graph has something to export.
//! 4. `draft_export_dwg` at R2004 (`AC1018`) — the renderer's default
//!    interchange version. Confirm the result reports two entities,
//!    the right version tag, and a non-zero file size.
//! 5. `draft_import_dwg` against the freshly written DWG file. The
//!    import is additive (the in-memory project graph keeps the
//!    original two primitives and the import re-adds them) — confirm
//!    the import reports the same two entities + the version tag
//!    matches the export.
//! 6. Repeat (4) + (5) against the R12 (`AC1009`) legacy version —
//!    this exercises the fixed-record codepath end-to-end.
//! 7. Confirm `draft_export_dwg` with an unknown / wrong-length AC
//!    tag surfaces a structured `Invalid` error (the renderer
//!    surfaces this as a typed toast, not a NAPI string-conversion
//!    failure).

use aec_bridge::{BridgeConfig, BridgeService};

fn write_template(root: &std::path::Path, category: &str, id: &str) {
    let category_dir = root.join(category);
    std::fs::create_dir_all(&category_dir).unwrap();
    let key = format!("{category}.{id}");
    let json = serde_json::json!({
        "template_id": key,
        "name": format!("DWG bridge fixture {id}"),
        "description": "Bridge DWG end-to-end fixture.",
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
    std::fs::write(category_dir.join(format!("{id}.json")), json.to_string()).unwrap();
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let projects = tmp.path().join("projects");
    let templates = tmp.path().join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    write_template(&templates, "interior", "studio");
    let cfg = BridgeConfig {
        state_dir: state,
        projects_dir: projects,
        templates_dir: templates,
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [7u8; 32]).unwrap();
    (s, tmp)
}

fn seed_two_primitives(s: &mut BridgeService, project_path: &str) {
    use aec_cad::primitives::{Circle, Line, Primitive};
    use aec_command::commands::draft::DrawPrimitive;
    use aec_command::commands::{Command, CommandKind};
    let line = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
        entity_id: aec_core::types::EntityId::new(),
        primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
    }));
    s.command_apply(project_path, line).expect("seed Line");
    let circle = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
        entity_id: aec_core::types::EntityId::new(),
        primitive: Primitive::Circle(Circle::new("0", [5.0, 5.0], 3.0)),
    }));
    s.command_apply(project_path, circle).expect("seed Circle");
}

#[test]
fn dwg_round_trip_through_bridge_r2004_default() {
    let (mut s, tmp) = boot_service();
    let summary = s
        .project_create_from_template("interior.studio", "DWG-Bridge-R2004")
        .unwrap();
    seed_two_primitives(&mut s, &summary.path);

    let dwg_out = tmp.path().join("export.dwg");
    let exp = s
        .draft_export_dwg(&summary.path, dwg_out.to_str().unwrap(), "AC1018")
        .expect("export R2004 DWG");
    assert_eq!(exp.entity_count, 2, "Line + Circle exported");
    assert_eq!(exp.version, "AC1018");
    assert!(exp.file_size > 0, "exported file is non-empty");
    assert!(dwg_out.exists(), "DWG file lands on disk");

    let imp = s
        .draft_import_dwg(&summary.path, dwg_out.to_str().unwrap())
        .expect("re-import R2004 DWG");
    // The import is additive: the existing two primitives stay in the
    // graph; the import re-creates two more from the file. The
    // counts the bridge reports are the per-import counts (NOT a
    // cumulative project-wide count), so the re-import reports 2.
    assert_eq!(
        imp.entity_count, 2,
        "re-import sees both exported primitives"
    );
    assert_eq!(imp.version, "AC1018", "version detected matches export");
    assert!(imp.layer_count >= 1, "at least the implicit \"0\" layer");
}

#[test]
fn dwg_round_trip_through_bridge_r12_legacy() {
    let (mut s, tmp) = boot_service();
    let summary = s
        .project_create_from_template("interior.studio", "DWG-Bridge-R12")
        .unwrap();
    seed_two_primitives(&mut s, &summary.path);

    let dwg_out = tmp.path().join("export-r12.dwg");
    let exp = s
        .draft_export_dwg(&summary.path, dwg_out.to_str().unwrap(), "AC1009")
        .expect("export R12 DWG");
    assert_eq!(exp.entity_count, 2);
    assert_eq!(exp.version, "AC1009");
    assert!(exp.file_size > 0);

    let imp = s
        .draft_import_dwg(&summary.path, dwg_out.to_str().unwrap())
        .expect("re-import R12 DWG");
    assert_eq!(imp.entity_count, 2);
    assert_eq!(imp.version, "AC1009");
}

#[test]
fn dwg_export_surfaces_structured_invalid_on_bad_version_tag() {
    let (mut s, tmp) = boot_service();
    let summary = s
        .project_create_from_template("interior.studio", "DWG-Bridge-BadTag")
        .unwrap();
    seed_two_primitives(&mut s, &summary.path);

    let dwg_out = tmp.path().join("never-written.dwg");

    // 5 bytes — wrong length.
    let err_short = s
        .draft_export_dwg(&summary.path, dwg_out.to_str().unwrap(), "AC101")
        .expect_err("5-byte signature must fail");
    let msg_short = format!("{err_short:?}");
    assert!(
        msg_short.contains("must be exactly 6"),
        "expected length-validation message, got `{msg_short}`"
    );
    assert!(
        !dwg_out.exists(),
        "no partial file is written on bad version"
    );

    // 6 bytes, but an unknown AC tag.
    let err_unknown = s
        .draft_export_dwg(&summary.path, dwg_out.to_str().unwrap(), "AC9999")
        .expect_err("unknown AC tag must fail");
    let msg_unknown = format!("{err_unknown:?}");
    assert!(
        msg_unknown.contains("unsupported"),
        "expected unsupported-version message, got `{msg_unknown}`"
    );
}
