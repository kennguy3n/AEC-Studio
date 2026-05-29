//! Phase 5 end-to-end user journey — "Render workflow" (Phase 12
//! Task 29). Driven entirely through the `BridgeService` public API.
//!
//! Steps:
//!
//!   1. Create project from the apartment template.
//!   2. "Save" 4 cameras — represented as deterministic camera ids
//!      that subsequent render jobs carry through to the queue. The
//!      bridge takes `camera_id: &str` opaquely; the journey owns the
//!      id ↔ name mapping in test scope to mirror what the renderer
//!      does with its `CameraStore`.
//!   3. Enqueue a batch render of the 4 cameras at the `standard`
//!      preset — exactly one job per camera via
//!      `render_enqueue_batch(cameras, &["standard"], None)`.
//!   4. Monitor progress: poll `render_batch_progress(batch_id)` and
//!      `render_list_jobs()` to verify the queue contains all four
//!      jobs in the `queued` state.
//!   5. Persist the queue to disk, drop the service, boot a *new*
//!      service, and restore the queue from the same path. Verify
//!      the new service sees all four jobs intact — this is the
//!      "render queue persists across service restart" claim.
//!   6. Inject two completed `RenderHistoryEntry`s into an
//!      `aec_render::history::RenderHistory`, then run
//!      `aec_render::history::compare(a, b)` and verify the resulting
//!      `CompareResult` flags the differences that the renderer
//!      surfaces in the "compare two renders" UI (preset / camera /
//!      image hash deltas + duration delta).
//!
//! The journey exercises the same bridge entry points the renderer
//! invokes via N-API, so a regression in queue shape / batch
//! aggregation / persistence surfaces here before it ships to the
//! renderer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aec_bridge::{BridgeConfig, BridgeService};
use aec_core::EntityId;
use aec_render::cameras::CameraSnapshot;
use aec_render::history::{compare, RenderHistory, RenderHistoryEntry};
use aec_render::job::{RenderJob, RenderJobStatus};
use aec_render::preset::RenderPreset;
use aec_render::scene::RenderScene;
use chrono::{TimeZone, Utc};

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

fn boot_service_in(state: &Path, projects: &Path, templates: &Path) -> BridgeService {
    std::fs::create_dir_all(templates).unwrap();
    copy_template("interior", "apartment", templates);
    let cfg = BridgeConfig {
        state_dir: state.to_path_buf(),
        projects_dir: projects.to_path_buf(),
        templates_dir: templates.to_path_buf(),
        max_recents: 10,
        extensions_dir: None,
    };
    BridgeService::new(cfg, [0x5Au8; 32]).expect("boot BridgeService")
}

fn camera(name: &str) -> CameraSnapshot {
    CameraSnapshot {
        id: EntityId::new(),
        name: name.to_string(),
        position_mm: [0.0, 0.0, 1500.0],
        target_mm: [0.0, 1000.0, 1500.0],
        up_mm: [0.0, 0.0, 1.0],
        focal_length_mm: 35.0,
        sensor_width_mm: 36.0,
        sensor_height_mm: 24.0,
        exposure_ev: 0.0,
        white_balance_k: 6500.0,
        aperture_f: 5.6,
        focus_distance_mm: 3000.0,
        aspect_ratio: 16.0 / 9.0,
        preset_key: None,
    }
}

/// Build a `RenderHistoryEntry` deterministically from a preset id,
/// camera, and an image_hash. The history entry is what the renderer
/// hands the bridge / saves to disk after a render completes; we
/// fabricate two here so the journey can exercise the
/// `compare(a, b)` surface without spinning up a real path tracer.
fn history_entry_with(
    job_id: &str,
    preset_id: &str,
    cam: Option<CameraSnapshot>,
    image_hash: Option<&str>,
    minutes: i64,
) -> RenderHistoryEntry {
    let mut preset = RenderPreset::standard();
    preset.id = preset_id.to_string();
    let mut job = RenderJob::new(preset, RenderScene::new());
    job.id = job_id.to_string();
    job.status = RenderJobStatus::Completed;
    job.progress = 1.0;
    job.created_at = Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap();
    job.completed_at = Some(
        Utc.with_ymd_and_hms(2026, 5, 19, 12, 0, 0).unwrap()
            + chrono::Duration::seconds(minutes * 60),
    );
    job.output_path = Some(PathBuf::from(format!("/tmp/{job_id}.png")));
    let mut entry = RenderHistoryEntry::from_job(&job).expect("job is completed");
    if let Some(c) = cam {
        entry = entry.with_camera(c);
    }
    if let Some(h) = image_hash {
        entry = entry.with_image_hash(h);
    }
    entry
}

#[test]
fn phase5_journey_batch_render_persistence_and_history_compare() {
    // ── Step 1: create project ──
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let projects_dir = tmp.path().join("projects");
    let templates_dir = tmp.path().join("templates");

    let summary;
    let camera_ids: Vec<String> = (0..4).map(|i| format!("cam_{i:02}")).collect();
    let queue_save = tmp.path().join("queue.json");

    let batch_id = {
        let mut svc = boot_service_in(&state_dir, &projects_dir, &templates_dir);
        summary = svc
            .project_create_from_template("interior.apartment", "Phase 5 Journey")
            .expect("create project");
        // ── Step 3: enqueue 4 cameras × standard preset ──
        let batch = svc
            .render_enqueue_batch(&camera_ids, &["standard".to_string()], None)
            .expect("render_enqueue_batch");
        assert_eq!(
            batch.job_ids.len(),
            4,
            "4 cameras × 1 preset = 4 jobs, got {}",
            batch.job_ids.len()
        );

        // ── Step 4: monitor progress ──
        // Right after enqueue the jobs are queued — no worker has
        // claimed any of them yet (no `__test_admit_*` was called).
        let progress = svc
            .render_batch_progress(&batch.batch_id)
            .expect("batch_progress")
            .expect("batch must exist immediately after enqueue");
        assert_eq!(
            progress.total, 4,
            "batch progress total must equal enqueued count"
        );
        assert_eq!(
            progress.queued, 4,
            "all four jobs should be queued before any admission"
        );

        let listed = svc.render_list_jobs().expect("list_jobs");
        assert_eq!(listed.len(), 4, "list_jobs must echo all 4 enqueued jobs");
        for job in &listed {
            assert_eq!(
                job.status, "queued",
                "no admission ran, so every job must be queued"
            );
        }

        // ── Step 5a: persist queue to disk ──
        svc.render_queue_persist(queue_save.to_str().unwrap())
            .expect("render_queue_persist");
        assert!(
            queue_save.exists(),
            "render_queue_persist must produce a file at {}",
            queue_save.display()
        );
        batch.batch_id
    }; // <-- svc drops; simulates a graceful shutdown.

    // ── Step 5b: boot a fresh service ──
    let _ = &summary;
    let svc2 = boot_service_in(&state_dir, &projects_dir, &templates_dir);
    // Brand-new service has an empty queue until restore.
    assert_eq!(
        svc2.render_list_jobs()
            .expect("list_jobs on fresh svc")
            .len(),
        0,
        "a freshly-booted service must start with an empty render queue"
    );

    // ── Step 5c: restore the queue ──
    svc2.render_queue_restore(queue_save.to_str().unwrap())
        .expect("render_queue_restore");
    let restored = svc2.render_list_jobs().expect("list_jobs after restore");
    assert_eq!(
        restored.len(),
        4,
        "restored queue must contain all 4 pre-shutdown jobs (got {})",
        restored.len()
    );
    // The batch id round-trips intact so the renderer can resume its
    // progress poll without re-enqueueing.
    let progress_after = svc2
        .render_batch_progress(&batch_id)
        .expect("batch_progress after restore")
        .expect("the batch id from before shutdown must still exist");
    assert_eq!(progress_after.total, 4);
    assert_eq!(progress_after.queued, 4);

    // ── Step 6: build & compare two render history entries ──
    let cam_a = camera("Master Bedroom");
    let cam_b = camera("Kitchen");
    let entry_a = history_entry_with(
        "job_a",
        "standard",
        Some(cam_a.clone()),
        Some("hash_aaaa"),
        5,
    );
    let entry_b = history_entry_with(
        "job_b",
        "high", // preset changed
        Some(cam_b.clone()),
        Some("hash_bbbb"), // image hash changed
        7,                 // 7 - 5 = 2 minutes longer
    );

    let mut history = RenderHistory::new();
    history.record(entry_a.clone());
    history.record(entry_b.clone());
    assert_eq!(history.len(), 2);

    let result = compare(&entry_a, &entry_b);
    assert_eq!(result.a_job_id, "job_a");
    assert_eq!(result.b_job_id, "job_b");
    assert!(
        result.preset_changed,
        "standard → high must register as a preset change"
    );
    assert!(
        result.camera_changed,
        "different camera snapshots must register as a camera change"
    );
    assert!(
        result.image_hash_changed,
        "differing image hashes must register as an image_hash change"
    );
    assert_eq!(
        result.duration_delta_ms,
        Duration::from_secs(2 * 60).as_millis() as i64,
        "duration delta must equal b.duration - a.duration"
    );
    assert_eq!(
        result.field_diffs.len(),
        3,
        "three changed fields (preset, camera, image_hash) must each surface as a FieldDiff"
    );
    // The renderer's compare UI displays these field labels verbatim.
    let labels: Vec<&str> = result
        .field_diffs
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    assert!(labels.contains(&"preset"));
    assert!(labels.contains(&"camera"));
    assert!(labels.contains(&"image_hash"));
}
