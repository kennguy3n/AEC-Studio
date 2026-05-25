//! Integration tests covering Phase 11 Group B — Task 12: real
//! template instantiation.
//!
//! These tests exercise `BridgeService::project_create_from_template`
//! against the *real shipped templates* (`templates/interior/*.json`,
//! `templates/architecture/*.json`, `templates/drafting/*.json`) and
//! verify that:
//!
//! * The on-disk SQLite project graph contains the right number of
//!   wall / floor / ceiling / room entities after instantiation
//!   (i.e. the commands actually wrote rows, not just emitted them).
//! * Multi-storey templates (e.g. `architecture.villa`) materialise
//!   every room across every storey.
//! * Sheet-only / layer-only templates (`drafting.2d_drafting`,
//!   `interior.renovation`) emit zero geometry entities.
//! * The audit sidecar at
//!   `<project>/audit/template_instantiation.json` is written and
//!   matches the materialised state.
//! * Undo over a template-instantiated project reverses the most
//!   recent command (e.g. the last `SaveCamera`).
//!
//! The tests use the project root reported by
//! `BridgeService::project_create_from_template`, then call
//! `project_graph_list` (the same path the renderer uses) to read
//! back what landed in SQLite.

use aec_bridge::{BridgeConfig, BridgeService};

fn templates_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates")
}

fn boot_service() -> (BridgeService, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: templates_root(),
        max_recents: 10,
    };
    let s = BridgeService::new(cfg, [7u8; 32]).unwrap();
    (s, tmp)
}

fn count_kind(s: &mut BridgeService, path: &str, kind: &str) -> usize {
    s.project_graph_list(path, Some(kind))
        .expect("graph_list")
        .len()
}

#[test]
fn apartment_template_lands_rooms_walls_floors_ceilings_on_graph() {
    let (mut s, _g) = boot_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Apartment Instantiate")
        .expect("create");

    // The shipped apartment template has 4 rooms. Each room becomes
    // 4 walls + 1 floor + 1 ceiling + 1 room => 7 entity rows.
    assert_eq!(count_kind(&mut s, &summary.path, "room"), 4);
    assert_eq!(count_kind(&mut s, &summary.path, "wall"), 16);
    assert_eq!(count_kind(&mut s, &summary.path, "floor"), 4);
    assert_eq!(count_kind(&mut s, &summary.path, "ceiling"), 4);
    // 3 camera presets in the JSON; one SaveCamera entity per preset.
    assert_eq!(count_kind(&mut s, &summary.path, "camera"), 3);

    // Sidecar exists and matches.
    let sidecar = std::path::Path::new(&summary.path)
        .join("audit")
        .join("template_instantiation.json");
    assert!(sidecar.exists(), "expected sidecar at {sidecar:?}");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["template_key"], "interior.apartment");
    assert_eq!(body["room_count"], 4);
    assert_eq!(body["camera_count"], 3);
    assert_eq!(body["lighting_preset"], "warm_evening");
    assert!(body["skipped"].as_array().unwrap().is_empty());
}

#[test]
fn villa_template_materialises_every_room_across_every_storey() {
    let (mut s, _g) = boot_service();
    let summary = s
        .project_create_from_template("architecture.villa", "Villa Instantiate")
        .expect("create");

    // The shipped villa template has 3 storeys with 2 + 5 + 4 = 11
    // rooms total. Every room is a 4-wall + 1-floor + 1-ceiling + 1-room
    // bundle.
    let n_rooms = count_kind(&mut s, &summary.path, "room");
    assert_eq!(n_rooms, 11, "expected 11 rooms across 3 storeys");
    assert_eq!(count_kind(&mut s, &summary.path, "wall"), 11 * 4);
    assert_eq!(count_kind(&mut s, &summary.path, "floor"), 11);
    assert_eq!(count_kind(&mut s, &summary.path, "ceiling"), 11);

    // Sidecar carries per-room storey attribution.
    let sidecar = std::path::Path::new(&summary.path)
        .join("audit")
        .join("template_instantiation.json");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    let rooms = body["rooms"].as_array().unwrap();
    assert_eq!(rooms.len(), 11);
    let storeys: Vec<&str> = rooms.iter().filter_map(|r| r["storey"].as_str()).collect();
    // Every materialised room carries a non-null storey name (none of
    // them came from the flat `rooms` list).
    assert_eq!(
        storeys.len(),
        11,
        "every villa room should carry a storey name"
    );
    assert!(storeys.contains(&"Basement"));
    assert!(storeys.contains(&"Ground"));
    assert!(storeys.contains(&"Upper"));
}

#[test]
fn drafting_template_emits_zero_geometry_entities() {
    let (mut s, _g) = boot_service();
    let summary = s
        .project_create_from_template("drafting.2d_drafting", "Drafting Instantiate")
        .expect("create");

    assert_eq!(count_kind(&mut s, &summary.path, "room"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "wall"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "floor"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "ceiling"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "camera"), 0);

    // Sidecar still gets written (positive signal: this path went
    // through the real template flow, just produced an empty batch).
    let sidecar = std::path::Path::new(&summary.path)
        .join("audit")
        .join("template_instantiation.json");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["template_key"], "drafting.2d_drafting");
    assert_eq!(body["room_count"], 0);
    assert_eq!(body["camera_count"], 0);
    assert_eq!(body["applied_command_count"], 0);
}

#[test]
fn renovation_template_is_geometryless_but_carries_lighting_preset() {
    let (mut s, _g) = boot_service();
    let summary = s
        .project_create_from_template("interior.renovation", "Renovation Instantiate")
        .expect("create");

    // No declared rooms => no walls / floors / ceilings / rooms in
    // the graph.
    assert_eq!(count_kind(&mut s, &summary.path, "room"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "wall"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "floor"), 0);
    assert_eq!(count_kind(&mut s, &summary.path, "ceiling"), 0);
    // No camera presets either.
    assert_eq!(count_kind(&mut s, &summary.path, "camera"), 0);

    // Lighting preset still flows into the sidecar so the renderer
    // can apply `daylight` even though no geometry was created.
    let sidecar = std::path::Path::new(&summary.path)
        .join("audit")
        .join("template_instantiation.json");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["template_key"], "interior.renovation");
    assert_eq!(body["lighting_preset"], "daylight");
}

#[test]
fn every_shipped_template_instantiates_cleanly() {
    let (mut s, _g) = boot_service();
    let templates = s.list_templates().expect("list templates");
    assert!(
        templates.len() >= 9,
        "expected >= 9 shipped templates, got {}",
        templates.len()
    );

    for choice in &templates {
        let project_name = format!("Smoke {}", choice.key);
        let summary = s
            .project_create_from_template(&choice.key, &project_name)
            .unwrap_or_else(|e| panic!("template {} failed to instantiate: {e}", choice.key));
        let sidecar = std::path::Path::new(&summary.path)
            .join("audit")
            .join("template_instantiation.json");
        assert!(
            sidecar.exists(),
            "template {} did not write the instantiation sidecar",
            choice.key
        );
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(body["template_key"], choice.key.as_str());
        // No template silently dropped rooms.
        assert!(
            body["skipped"].as_array().unwrap().is_empty(),
            "template {} silently skipped rooms: {}",
            choice.key,
            body["skipped"]
        );

        // For any template that materialised at least one room, the
        // database row counts must match the sidecar's `room_count` /
        // `camera_count` exactly. This catches a (theoretical) case
        // where the SQL transaction committed only a subset of the
        // batch.
        let expected_rooms = body["room_count"].as_u64().unwrap() as usize;
        let expected_cameras = body["camera_count"].as_u64().unwrap() as usize;
        assert_eq!(count_kind(&mut s, &summary.path, "room"), expected_rooms);
        assert_eq!(
            count_kind(&mut s, &summary.path, "wall"),
            expected_rooms * 4
        );
        assert_eq!(count_kind(&mut s, &summary.path, "floor"), expected_rooms);
        assert_eq!(count_kind(&mut s, &summary.path, "ceiling"), expected_rooms);
        assert_eq!(
            count_kind(&mut s, &summary.path, "camera"),
            expected_cameras
        );
    }
}

#[test]
fn undo_over_a_template_instantiated_project_reverses_last_command() {
    use aec_core::types::Scope;
    let (mut s, _g) = boot_service();
    let summary = s
        .project_create_from_template("interior.apartment", "Undo After Template")
        .expect("create");

    // The apartment template ends with 3 SaveCamera commands; undo
    // once and the last camera should be gone.
    let cameras_before = count_kind(&mut s, &summary.path, "camera");
    assert_eq!(cameras_before, 3);
    let undo = s.command_undo(&summary.path, Scope::Design).expect("undo");
    assert_eq!(undo.applied.len(), 1, "undo reverses one command");
    let cameras_after = count_kind(&mut s, &summary.path, "camera");
    assert_eq!(
        cameras_after,
        cameras_before - 1,
        "undo should drop one camera entity",
    );
}
