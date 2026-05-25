//! Phase 11 Task 23 acceptance test:
//!
//! > Test: enqueue 4 jobs → simulate crash after job 2 → restart →
//! > verify jobs 3-4 are still queued and resumable.
//!
//! The integration test below drives the full `RenderJobStore` +
//! `RenderQueue` interaction through two process-lifecycle phases
//! (pre-crash, post-restart) and verifies both the queue and the
//! single-job APIs see the same state.

use std::path::PathBuf;

use aec_render::{
    queue::RenderQueue, RenderJob, RenderJobStatus, RenderJobStore, RenderPreset, RenderScene,
};

fn fixture(path: PathBuf) -> RenderJobStore {
    RenderJobStore::open(path).unwrap()
}

#[test]
fn full_lifecycle_four_jobs_crash_after_two_complete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("renders").join("queue.sqlite");

    // --- Pre-crash phase ---
    let job_ids: Vec<String> = {
        let store = fixture(path.clone());
        let mut q = RenderQueue::new();
        let mut ids = Vec::new();
        for _ in 0..4 {
            let job = RenderJob::new(RenderPreset::standard(), RenderScene::new());
            ids.push(job.id.clone());
            q.submit(job.clone());
            store.upsert(&job).unwrap();
        }

        // Job 1: admit + complete.
        let j1 = q.admit().unwrap();
        // Reflect transition in the store.
        let mut row = store.get(&j1.id).unwrap().unwrap();
        row.status = RenderJobStatus::Running;
        store.upsert(&row).unwrap();
        q.complete(&j1.id, "/tmp/j1.png").unwrap();
        let done = q.get(&j1.id).unwrap();
        store.upsert(done).unwrap();

        // Job 2: admit + complete.
        let j2 = q.admit().unwrap();
        let mut row = store.get(&j2.id).unwrap().unwrap();
        row.status = RenderJobStatus::Running;
        store.upsert(&row).unwrap();
        q.complete(&j2.id, "/tmp/j2.png").unwrap();
        let done = q.get(&j2.id).unwrap();
        store.upsert(done).unwrap();

        // Job 3: admit + simulate mid-render progress, then "crash"
        // (drop the store + queue).
        let j3 = q.admit().unwrap();
        let mut row3 = store.get(&j3.id).unwrap().unwrap();
        row3.status = RenderJobStatus::Running;
        row3.progress = 0.42;
        store.upsert(&row3).unwrap();

        ids
    }; // store/queue dropped → simulates process exit.

    // --- Post-restart phase ---
    let store = fixture(path.clone());
    // The OS-level WAL must have flushed; the data we wrote before
    // the implicit drop survives.
    let counts = store.count_by_status().unwrap();
    assert_eq!(counts.completed, 2, "two completed jobs survived");
    assert_eq!(counts.running, 1, "the mid-render job is still 'running'");
    assert_eq!(counts.queued, 1, "the never-started job is still queued");
    assert_eq!(counts.total(), 4);

    // Crash recovery: flip the running rows back to queued. The
    // worker pool will pull them on its next admit cycle. Progress
    // is preserved so the worker can skip already-finished tiles.
    let n_resurrected = store.reincarnate_running_as_queued().unwrap();
    assert_eq!(n_resurrected, 1);

    let counts = store.count_by_status().unwrap();
    assert_eq!(counts.queued, 2, "one original + one resurrected");
    assert_eq!(counts.running, 0);
    assert_eq!(counts.completed, 2);

    // Rebuild the in-memory queue and verify both pending jobs are
    // there and resumable (i.e. `admit()` returns them).
    let mut q = store.load_queue().unwrap();
    assert_eq!(
        q.list_jobs().len(),
        4,
        "every job — completed too — round-trips into the queue"
    );

    // We should be able to admit two queued jobs to running.
    let a = q.admit().unwrap();
    let b = q.admit().unwrap();
    assert_eq!(a.status, RenderJobStatus::Running);
    assert_eq!(b.status, RenderJobStatus::Running);

    // No more queued jobs to admit.
    assert!(q.admit().is_none());

    // The resurrected job (id == ids[2]) is one of the two we just
    // admitted, and its preserved progress is non-zero.
    let resurrected = q.get(&job_ids[2]).unwrap();
    assert_eq!(resurrected.status, RenderJobStatus::Running);
    assert!(
        (resurrected.progress - 0.42).abs() < 1e-5,
        "preserved mid-render progress, got {}",
        resurrected.progress
    );
}

#[test]
fn store_supports_concurrent_upserts_from_multiple_workers() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = Arc::new(RenderJobStore::open(&path).unwrap());

    // Pre-populate 10 queued jobs.
    let mut ids = Vec::new();
    for _ in 0..10 {
        let j = RenderJob::new(RenderPreset::standard(), RenderScene::new());
        ids.push(j.id.clone());
        store.upsert(&j).unwrap();
    }

    // Spawn 4 workers that each flip 2 jobs Running → Completed.
    let chunks: Vec<Vec<String>> = ids.chunks(2).map(<[_]>::to_vec).collect();
    let mut handles = Vec::new();
    for chunk in chunks.into_iter().take(4) {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for id in &chunk {
                let mut j = store.get(id).unwrap().unwrap();
                j.status = RenderJobStatus::Running;
                store.upsert(&j).unwrap();
                j.status = RenderJobStatus::Completed;
                j.progress = 1.0;
                store.upsert(&j).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let c = store.count_by_status().unwrap();
    assert_eq!(c.completed, 8, "4 workers × 2 jobs each = 8");
    assert_eq!(c.queued, 2);
}

#[test]
fn round_trip_through_save_queue_load_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queue.sqlite");
    let store = RenderJobStore::open(&path).unwrap();

    let mut q = RenderQueue::new();
    let mut ids = Vec::new();
    for p in [10, 5, 0] {
        ids.push(
            q.submit(RenderJob::new(RenderPreset::standard(), RenderScene::new()).with_priority(p)),
        );
    }
    // Drive one job to running.
    let admitted = q.admit().unwrap();
    let running_id = admitted.id.clone();

    store.save_queue(&q).unwrap();
    let loaded = store.load_queue().unwrap();

    assert_eq!(loaded.list_jobs().len(), 3);
    assert_eq!(
        loaded.get(&running_id).unwrap().status,
        RenderJobStatus::Running
    );

    // Priority order survives: highest priority is the running one
    // (it was admitted from the head); the next admit() on `loaded`
    // should yield the priority-5 job.
    let mut loaded = loaded;
    let next = loaded.admit().unwrap();
    assert_eq!(next.priority, 5);
}
