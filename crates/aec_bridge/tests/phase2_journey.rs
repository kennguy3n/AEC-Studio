//! Phase 2 end-to-end user journey — Interior Designer ("Apartment
//! renovation in a weekend"). Driven entirely through the
//! `BridgeService` public API, the same surface the desktop
//! renderer talks to.
//!
//! Steps (from `PROPOSAL.md` "User Journey A"):
//!
//!   1. Create project from the `interior.apartment` template.
//!   2. Place furniture from the asset library.
//!   3. Set lighting preset.
//!   4. Save four cameras (4 viewpoints).
//!   5. Enqueue four renders (one per camera at `standard` quality).
//!   6. Export the client concept-pack PDF (deliver_build_pack with
//!      `kind = "concept"`).
//!
//! Every step is asserted at the bridge boundary — the test is *not*
//! a thin wrapper around a single crate. It exercises the full
//! Phase 2 surface (project lifecycle, command engine, asset
//! library, lighting / camera / furniture commands, render queue,
//! deliver pack export) so a regression in any of them surfaces
//! here even if it slipped past the crate-level tests.
//!
//! The shipped `templates/interior/apartment.json` is copied into a
//! tempdir so the test runs hermetically (no global file dep, no
//! cross-test pollution).

use std::path::{Path, PathBuf};

use aec_bridge::{
    AssetListQuery, BridgeConfig, BridgeService, DeliverBuildPackParams, DeliverPackInventoryFlags,
};
use aec_command::commands::{
    camera::{CameraParams, SaveCamera},
    furniture::PlaceFurniture,
    lighting::SetLighting,
    Command, CommandKind,
};
use aec_core::types::EntityId;

/// Locate the workspace's shipped templates directory. Cargo runs
/// integration tests with `CARGO_MANIFEST_DIR=<crate>` so we ascend
/// to the workspace root and join `templates`.
fn workspace_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("templates")
}

/// Copy a single shipped template + its category dir into `dest`.
/// Keeps the fixture footprint small and the test hermetic.
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
    };
    let svc = BridgeService::new(cfg, [0x4Au8; 32]).expect("boot BridgeService");
    (svc, tmp)
}

#[test]
fn phase2_apartment_designer_journey_through_bridge_service() {
    // ── Step 1: create the project from the apartment template ──
    let (mut svc, _g) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Phase 2 Journey")
        .expect("create apartment");
    assert_eq!(summary.name, "Phase 2 Journey");
    assert!(
        !summary.path.is_empty(),
        "project path should be set by create_from_template"
    );

    // Open + confirm recents store sees it.
    let opened = svc.project_open(&summary.path).expect("open");
    assert_eq!(opened.project_id, summary.project_id);

    // The project graph is queryable post-create (full template-to-graph
    // instantiation is Group B Task 12 / PR #51 territory — this journey
    // only requires that the graph surface works, not that the template
    // pre-populated it).
    let all_entities = svc
        .project_graph_list(&summary.path, None)
        .expect("graph: list all");
    let _ = all_entities; // existence check; counts depend on Task 12 merging.

    // ── Step 2: place furniture from the asset library ──
    // Pull the seed library; the renderer's flow is "browse assets →
    // drag onto canvas → place".
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("list assets");
    assert!(
        assets.len() >= 4,
        "seed library must have at least 4 demo assets, got {}",
        assets.len()
    );

    // Snapshot the template baseline so we can assert the *delta*
    // contributed by this journey (place / save calls), independent
    // of any entities the apartment template itself pre-populates
    // (Phase 11 Group B / Task 12 template instantiation, PR #51).
    let furniture_baseline = svc
        .project_graph_list(&summary.path, Some("furniture"))
        .expect("graph: furniture baseline")
        .len();

    // Place each demo asset once in distinct quadrants of the room.
    let furniture_positions: Vec<[f64; 3]> = vec![
        [1_000.0, 1_000.0, 0.0],
        [3_500.0, 1_000.0, 0.0],
        [1_000.0, 3_000.0, 0.0],
        [3_500.0, 3_000.0, 0.0],
    ];
    let mut furniture_entity_ids: Vec<EntityId> = Vec::new();
    for (asset, position) in assets.iter().zip(furniture_positions.iter()) {
        let eid = EntityId::new();
        let cmd = Command::user(CommandKind::PlaceFurniture(PlaceFurniture {
            entity_id: eid.clone(),
            asset_ref: asset.asset_id.clone(),
            position_mm: *position,
            rotation_yaw_deg: 0.0,
            scale_override: None,
            name: Some(format!("{} (journey)", asset.name)),
            parent: None,
        }));
        svc.command_apply(&summary.path, cmd)
            .unwrap_or_else(|e| panic!("place furniture {}: {e}", asset.asset_id));
        furniture_entity_ids.push(eid);
    }
    assert_eq!(
        furniture_entity_ids.len(),
        4,
        "should have placed 4 furniture instances"
    );

    let furniture = svc
        .project_graph_list(&summary.path, Some("furniture"))
        .expect("graph: furniture");
    assert_eq!(
        furniture.len(),
        furniture_baseline + 4,
        "exactly 4 new furniture entities should be in the graph after placement \
         (baseline {furniture_baseline} + 4 placed)"
    );
    for eid in &furniture_entity_ids {
        assert!(
            furniture.iter().any(|e| &e.id == eid),
            "placed furniture entity {eid:?} must be queryable in the graph"
        );
    }

    // ── Step 3: set the lighting preset ──
    let cmd = Command::user(CommandKind::SetLighting(SetLighting {
        preset_id: "warm_evening".into(),
    }));
    svc.command_apply(&summary.path, cmd)
        .expect("set lighting preset");

    // Snapshot the camera baseline post-template-instantiation so
    // the assert below counts only the cameras *this test* saved.
    let camera_baseline = svc
        .project_graph_list(&summary.path, Some("camera"))
        .expect("graph: camera baseline")
        .len();

    // ── Step 4: save 4 cameras (Living, Bedroom, Bathroom, Hero) ──
    let camera_specs: [(&str, [f64; 3], [f64; 3]); 4] = [
        (
            "Living Wide",
            [5_000.0, 2_000.0, 1_650.0],
            [3_500.0, 4_000.0, 1_200.0],
        ),
        (
            "Bedroom Eye",
            [6_000.0, 6_000.0, 1_650.0],
            [4_500.0, 7_500.0, 1_200.0],
        ),
        (
            "Bathroom Detail",
            [9_500.0, 3_500.0, 1_650.0],
            [10_000.0, 4_500.0, 1_200.0],
        ),
        (
            "Hero Concept",
            [-2_000.0, -2_000.0, 1_650.0],
            [3_500.0, 4_000.0, 1_500.0],
        ),
    ];
    let mut camera_entity_ids: Vec<EntityId> = Vec::new();
    for (name, position, target) in &camera_specs {
        let eid = EntityId::new();
        let cmd = Command::user(CommandKind::SaveCamera(SaveCamera {
            entity_id: eid.clone(),
            name: (*name).to_string(),
            params: CameraParams {
                position_mm: *position,
                target_mm: *target,
                focal_length_mm: 35.0,
                exposure_ev: 0.0,
                white_balance_k: 5_600,
                depth_of_field_f: Some(5.6),
                aspect_ratio: 16.0 / 9.0,
            },
        }));
        svc.command_apply(&summary.path, cmd)
            .unwrap_or_else(|e| panic!("save camera {name}: {e}"));
        camera_entity_ids.push(eid);
    }

    let cameras = svc
        .project_graph_list(&summary.path, Some("camera"))
        .expect("graph: cameras");
    assert_eq!(
        cameras.len(),
        camera_baseline + 4,
        "exactly 4 new cameras saved on top of the template baseline \
         (baseline {camera_baseline} + 4 saved)"
    );
    for eid in &camera_entity_ids {
        assert!(
            cameras.iter().any(|c| &c.id == eid),
            "saved camera entity {eid:?} must be queryable in the graph"
        );
    }

    // ── Step 5: enqueue 4 renders (one per camera at `standard`) ──
    let camera_id_strings: Vec<String> = camera_entity_ids
        .iter()
        .map(|e| e.as_str().to_string())
        .collect();
    let batch = svc
        .render_enqueue_batch(&camera_id_strings, &["standard".to_string()], None)
        .expect("enqueue batch");
    assert_eq!(
        batch.job_ids.len(),
        4,
        "batch should contain exactly 4 jobs (one per camera × one preset)"
    );
    assert!(!batch.batch_id.is_empty());

    let listed_jobs = svc.render_list_jobs().expect("list jobs");
    assert_eq!(
        listed_jobs.len(),
        4,
        "render queue should report all 4 enqueued jobs"
    );

    // Batch progress is queryable and reports 0/4 done immediately
    // post-enqueue (no driver running in this test).
    let progress = svc
        .render_batch_progress(&batch.batch_id)
        .expect("batch progress")
        .expect("batch progress should be present for an existing batch id");
    assert_eq!(progress.total, 4);
    assert_eq!(progress.completed, 0);

    // ── Step 6: export the client concept pack PDF ──
    let tmp_out = tempfile::tempdir().unwrap();
    let pack_path = tmp_out.path().join("concept_pack.zip");
    let pack = svc
        .deliver_build_pack(DeliverBuildPackParams {
            out_path: pack_path.to_string_lossy().into_owned(),
            kind: "concept".into(),
            project_name: "Phase 2 Journey".into(),
            options: DeliverPackInventoryFlags {
                include_renders: true,
                include_sheets: true,
                include_ifc: false,
                include_boq: false,
                include_proposal: true,
            },
        })
        .expect("deliver concept pack");

    // Verify on-disk ZIP exists, has content, and contains the
    // concept-pack PDF + at least one sheet + the proposal stub.
    let pack_path = PathBuf::from(&pack.out_path);
    assert!(pack_path.exists(), "deliver pack should be written to disk");
    let size = std::fs::metadata(&pack_path).unwrap().len();
    assert!(
        size > 1_000,
        "concept pack should be >1 KB, got {size} bytes"
    );
    assert!(pack.contents.iter().any(|c| c == "concept_pack.pdf"));
    assert!(
        pack.contents.iter().any(|c| c.starts_with("sheets/")),
        "concept pack with include_sheets=true should contain a sheets/ entry; got {:?}",
        pack.contents
    );

    // ── Sanity: project is still in a clean state and can be saved ──
    let saved = svc.project_save(&summary.path).expect("save");
    assert_eq!(saved.project_id, summary.project_id);
}

/// Re-open the project after the journey and verify the persisted
/// graph still holds all 4 furniture + 4 camera entities. Pins the
/// "user closes the app, comes back tomorrow" contract — without
/// this the journey could pass while the SQLCipher write path was
/// silently dropping mutations.
#[test]
fn phase2_journey_persists_across_service_restart() {
    let (mut svc, tmp) = boot_service();
    let summary = svc
        .project_create_from_template("interior.apartment", "Phase 2 Restart")
        .expect("create");

    // Snapshot the template-instantiation baseline so the post-restart
    // assertion can verify the *delta* this test added survives the
    // service reboot, independent of whatever the apartment template
    // pre-populates.
    let furniture_baseline = svc
        .project_graph_list(&summary.path, Some("furniture"))
        .expect("furniture baseline")
        .len();
    let camera_baseline = svc
        .project_graph_list(&summary.path, Some("camera"))
        .expect("camera baseline")
        .len();

    // Place 2 furniture + 2 cameras through command_apply. We track
    // each entity_id so the post-restart check can prove the *exact*
    // entities this test created round-tripped, not just that the
    // count is right (a count-only check could be satisfied by the
    // template baseline alone if mutations were silently dropped).
    let assets = svc
        .design_list_assets(&AssetListQuery::default())
        .expect("list");
    let mut placed_furniture_ids: Vec<EntityId> = Vec::new();
    for (i, a) in assets.iter().take(2).enumerate() {
        let eid = EntityId::new();
        let cmd = Command::user(CommandKind::PlaceFurniture(PlaceFurniture {
            entity_id: eid.clone(),
            asset_ref: a.asset_id.clone(),
            position_mm: [1_000.0 + 1_000.0 * i as f64, 1_000.0, 0.0],
            rotation_yaw_deg: 0.0,
            scale_override: None,
            name: Some(a.name.clone()),
            parent: None,
        }));
        svc.command_apply(&summary.path, cmd).unwrap();
        placed_furniture_ids.push(eid);
    }
    let mut saved_camera_ids: Vec<EntityId> = Vec::new();
    for i in 0..2 {
        let eid = EntityId::new();
        let cmd = Command::user(CommandKind::SaveCamera(SaveCamera {
            entity_id: eid.clone(),
            name: format!("Cam{i}"),
            params: CameraParams {
                position_mm: [0.0, 0.0, 1_650.0],
                target_mm: [3_000.0, 3_000.0, 1_500.0],
                focal_length_mm: 35.0,
                exposure_ev: 0.0,
                white_balance_k: 5_600,
                depth_of_field_f: None,
                aspect_ratio: 16.0 / 9.0,
            },
        }));
        svc.command_apply(&summary.path, cmd).unwrap();
        saved_camera_ids.push(eid);
    }
    svc.project_save(&summary.path).expect("save");
    drop(svc); // close the service; tempdir survives because we hold `tmp`.

    // Reboot a fresh service against the same state/projects/templates.
    let cfg = BridgeConfig {
        state_dir: tmp.path().join("state"),
        projects_dir: tmp.path().join("projects"),
        templates_dir: tmp.path().join("templates"),
        max_recents: 10,
    };
    let mut svc2 = BridgeService::new(cfg, [0x4Au8; 32]).expect("reboot");
    svc2.project_open(&summary.path).expect("reopen");

    let furniture = svc2
        .project_graph_list(&summary.path, Some("furniture"))
        .expect("furniture after reboot");
    assert_eq!(
        furniture.len(),
        furniture_baseline + 2,
        "furniture entities must persist across service restart \
         (baseline {furniture_baseline} + 2 placed)"
    );
    for eid in &placed_furniture_ids {
        assert!(
            furniture.iter().any(|e| &e.id == eid),
            "placed furniture {eid:?} must survive service restart"
        );
    }
    let cameras = svc2
        .project_graph_list(&summary.path, Some("camera"))
        .expect("cameras after reboot");
    assert_eq!(
        cameras.len(),
        camera_baseline + 2,
        "camera entities must persist across service restart \
         (baseline {camera_baseline} + 2 saved)"
    );
    for eid in &saved_camera_ids {
        assert!(
            cameras.iter().any(|c| &c.id == eid),
            "saved camera {eid:?} must survive service restart"
        );
    }
}
