//! End-to-end integration tests for Phase 5 (Render pipeline hardening).
//!
//! These tests exercise the public API of `aec_render` as the rest of the
//! workspace would use it. The goal is to validate Phase 5 exit
//! criteria from `PROGRESS.md`:
//!
//! * Render queue resume on failure — single jobs and walkthroughs.
//! * Render history surfaces a usable before/after compare across
//!   distinct renders.
//! * Batch + matrix submission flow funnels through the same admit /
//!   complete / fail / resume loop the UI exercises.
//!
//! None of these tests touch Blender or a real GPU; they only drive the
//! Rust-side state machine, which is exactly what we need to validate
//! the queue + history + resume contract.

use std::path::PathBuf;

use aec_core::EntityId;
use aec_render::{
    cameras::CameraSnapshot,
    history::{compare, RenderHistory, RenderHistoryEntry},
    job::{RenderJob, RenderJobStatus},
    preset::RenderPreset,
    queue::RenderQueue,
    scene::RenderScene,
};

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

/// Phase 5 exit criterion: "queue 8 renders; fail 2; resume them
/// successfully". This exercises the full lifecycle including the
/// admit -> running -> fail -> resume -> admit-again loop and
/// asserts the queue's final state has all 8 completed.
#[test]
fn queue_eight_renders_fail_two_and_resume_them() {
    let mut q = RenderQueue::new();
    let scene = RenderScene::new();

    // Submit 8 standard renders, each tied to a distinct camera so the
    // queue admits them in deterministic order.
    let cameras: Vec<_> = (0..8).map(|i| camera(&format!("cam_{i}"))).collect();
    let submission = q.submit_batch(&cameras, RenderPreset::standard(), &scene);
    assert_eq!(submission.job_ids.len(), 8);
    assert_eq!(q.queued_count(), 8);

    // Drain the queue: admit each job, complete most, fail #3 and #5.
    let mut all_ids: Vec<String> = Vec::new();
    let failed_indices = [2usize, 4];
    for i in 0..8 {
        let job = q.admit().expect("queue should admit a job");
        all_ids.push(job.id.clone());
        if failed_indices.contains(&i) {
            q.fail(&job.id, format!("synthetic crash at job {i}"))
                .expect("fail should succeed");
        } else {
            q.complete(&job.id, format!("out_{i}.png"))
                .expect("complete should succeed");
        }
    }
    assert_eq!(q.completed_count(), 8, "8 jobs reached a terminal state");

    // Two are in the Failed sub-state; everything else is Completed.
    let failed_jobs: Vec<&RenderJob> = q
        .list_jobs()
        .into_iter()
        .filter(|j| matches!(j.status, RenderJobStatus::Failed))
        .collect();
    assert_eq!(failed_jobs.len(), 2);
    let failed_ids: Vec<String> = failed_jobs.iter().map(|j| j.id.clone()).collect();

    // Resume each failed job: it should re-enter the queue and admit
    // again. Then complete it for real.
    for id in &failed_ids {
        q.resume(id).expect("resume should succeed for failed job");
    }
    assert_eq!(q.queued_count(), 2);
    for _ in 0..2 {
        let job = q.admit().expect("resumed job should admit");
        q.complete(&job.id, format!("retry_{}.png", job.id))
            .unwrap();
    }

    // Final state: all 8 are Completed, none Failed.
    assert_eq!(q.queued_count(), 0);
    assert_eq!(q.running_count(), 0);
    let still_failed = q
        .list_jobs()
        .into_iter()
        .filter(|j| matches!(j.status, RenderJobStatus::Failed))
        .count();
    assert_eq!(still_failed, 0, "no failures left after resume");
    let total_completed = q
        .list_jobs()
        .into_iter()
        .filter(|j| matches!(j.status, RenderJobStatus::Completed))
        .count();
    assert_eq!(total_completed, 8);
}

/// Phase 5 exit criterion: render history surfaces a meaningful
/// before/after compare. We record two completed renders shot from
/// different cameras and confirm `compare` finds the camera diff.
#[test]
fn render_history_surfaces_before_after_compare() {
    let mut q = RenderQueue::new();
    let scene = RenderScene::new();
    let cam_a = camera("Wide Living");
    let cam_b = camera("Eye-Level Living");

    // Two renders, identical preset, different cameras.
    let id_a = q.submit(
        RenderJob::new(RenderPreset::standard(), scene.clone())
            .with_camera_id(cam_a.id.to_string()),
    );
    let id_b = q.submit(
        RenderJob::new(RenderPreset::standard(), scene.clone())
            .with_camera_id(cam_b.id.to_string()),
    );

    let job_a = q.admit().unwrap();
    assert_eq!(job_a.id, id_a);
    q.complete(&id_a, "render_a.png").unwrap();
    let job_b = q.admit().unwrap();
    assert_eq!(job_b.id, id_b);
    q.complete(&id_b, "render_b.png").unwrap();

    let mut history = RenderHistory::new();
    // Move each finished job into the history with its camera attached.
    let finished_a = q.get(&id_a).unwrap();
    let finished_b = q.get(&id_b).unwrap();
    let entry_a = RenderHistoryEntry::from_job(finished_a)
        .unwrap()
        .with_camera(cam_a.clone())
        .with_image_hash("blake3:aaa");
    let entry_b = RenderHistoryEntry::from_job(finished_b)
        .unwrap()
        .with_camera(cam_b.clone())
        .with_image_hash("blake3:bbb");
    history.record(entry_a.clone());
    history.record(entry_b.clone());
    assert_eq!(history.len(), 2);

    let cmp = compare(&entry_a, &entry_b);
    assert!(cmp.camera_changed, "compare must flag camera change: {cmp:?}");
    assert!(
        cmp.image_hash_changed,
        "compare must flag image hash change: {cmp:?}"
    );
    assert!(
        cmp.field_diffs.iter().any(|f| f.field == "camera"),
        "field_diffs must include camera entry: {cmp:?}"
    );
    assert!(
        cmp.field_diffs.iter().any(|f| f.field == "image_hash"),
        "field_diffs must include image_hash entry: {cmp:?}"
    );
    // Both entries are reachable from the history index.
    assert!(history.get(&id_a).is_some());
    assert!(history.get(&id_b).is_some());

    // `by_recency` orders newest-first, so the most recently completed
    // job (entry_b) should appear before entry_a.
    let recency: Vec<&str> = history
        .by_recency()
        .iter()
        .map(|e| e.job_id.as_str())
        .collect();
    assert_eq!(recency.len(), 2);
    // Both ids must be present; ordering can vary if completed_at
    // ticks land in the same instant, so assert containment rather
    // than strict ordering.
    assert!(recency.contains(&id_a.as_str()));
    assert!(recency.contains(&id_b.as_str()));
}

/// Phase 5 exit criterion: walkthrough resumes from the last completed
/// frame. We stamp a job with 15 completed frames, fail it, resume it,
/// and assert the next frame to render is frame 16 (i.e. last + 1).
#[test]
fn walkthrough_resumes_from_last_completed_frame() {
    let mut q = RenderQueue::new();
    let scene = RenderScene::new();

    let mut job = RenderJob::new(RenderPreset::walkthrough(), scene);
    // Simulate the worker having flushed frames 0..=14 before crashing.
    for f in 0..15 {
        job.mark_frame_completed(f);
    }
    let id = q.submit(job);
    let _ = q.admit().unwrap();
    q.fail(&id, "frame 15 OOM crash").unwrap();

    // Resume keeps the completed_frames marker so the worker resumes
    // at the next gap.
    q.resume(&id).unwrap();
    let resumed = q.get(&id).unwrap();
    assert_eq!(resumed.status, RenderJobStatus::Queued);
    assert_eq!(resumed.completed_frames.last().copied(), Some(14));
    assert_eq!(resumed.next_walkthrough_frame(), Some(15));

    // The queue should re-admit the resumed job in walkthrough mode.
    let readmit = q.admit().expect("resumed walkthrough should admit");
    assert_eq!(readmit.id, id);
    assert_eq!(readmit.status, RenderJobStatus::Running);
    assert_eq!(readmit.completed_frames.last().copied(), Some(14));
}

/// Phase 5: queue state survives a serialize / deserialize roundtrip.
/// This is the contract that backs the "render queue resume across app
/// crashes" exit criterion in PROGRESS.md.
#[test]
fn queue_state_round_trips_through_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let path: PathBuf = tmp.path().join("queue_state.json");

    let mut original = RenderQueue::new();
    let scene = RenderScene::new();
    let cameras: Vec<_> = (0..3).map(|i| camera(&format!("c_{i}"))).collect();
    let _ = original.submit_batch(&cameras, RenderPreset::quick(), &scene);
    // Admit one and fail it so the persisted state has a non-trivial
    // queue + completed split to round-trip.
    let admitted = original.admit().unwrap();
    original.fail(&admitted.id, "to disk").unwrap();

    original.persist(&path).unwrap();
    let reloaded = RenderQueue::load(&path).unwrap();

    assert_eq!(reloaded.list_jobs().len(), original.list_jobs().len());
    let original_failed = original
        .list_jobs()
        .iter()
        .filter(|j| matches!(j.status, RenderJobStatus::Failed))
        .count();
    let reloaded_failed = reloaded
        .list_jobs()
        .iter()
        .filter(|j| matches!(j.status, RenderJobStatus::Failed))
        .count();
    assert_eq!(original_failed, reloaded_failed);
}

/// Phase 5: matrix submission (cameras × presets) creates the expected
/// fan-out and every job carries the matching batch id.
#[test]
fn matrix_submission_fans_out_cameras_times_presets() {
    let mut q = RenderQueue::new();
    let scene = RenderScene::new();
    let cams: Vec<_> = (0..3).map(|i| camera(&format!("mc_{i}"))).collect();
    let presets = vec![RenderPreset::standard(), RenderPreset::high()];
    let sub = q.submit_matrix(&cams, &presets, &scene);

    assert_eq!(sub.job_ids.len(), cams.len() * presets.len());
    let jobs_in_batch = q.jobs_in_batch(&sub.batch_id);
    assert_eq!(jobs_in_batch.len(), cams.len() * presets.len());
    let progress = q.batch_progress(&sub.batch_id).unwrap();
    assert_eq!(progress.total, cams.len() * presets.len());
    assert_eq!(progress.queued, cams.len() * presets.len());
    assert_eq!(progress.completed, 0);
}
