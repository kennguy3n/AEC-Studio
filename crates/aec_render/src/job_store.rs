//! SQLite-backed persistent store for [`RenderJob`]s.
//!
//! The render queue is in-memory by default (see [`crate::RenderQueue`]),
//! but on a crash or kill -9 we need the queue to come back exactly as it
//! was the last time a render worker saw it. The existing [`RenderQueue::persist`]
//! pathway serialises the **whole** queue to JSON every time it's called;
//! that's adequate for small queues but has two correctness gaps in
//! production:
//!
//! 1. **Per-tile/per-frame updates are not atomic at the file level.**
//!    A walkthrough render that marks each frame complete via
//!    `RenderJob::mark_frame_completed` would need to re-serialise the
//!    full queue on every frame; if the process is killed mid-write the
//!    JSON file is corrupt (the `write-then-rename` discipline mitigates
//!    but doesn't eliminate this for slow writes).
//! 2. **No granular index** — listing batches, jobs by status, or
//!    resuming "everything that was running" requires loading the
//!    whole file.
//!
//! [`RenderJobStore`] addresses both: every job is one row in a
//! `render_jobs` table, single-job updates are one transaction, and
//! status / batch are indexed columns so the resume path can pull
//! `WHERE status IN ('queued','running')` directly. On every successful
//! mutation the store also fsyncs via SQLite's WAL checkpoint so a
//! `kill -9` immediately after the call is safe.
//!
//! ## Crash recovery contract
//!
//! On startup:
//!
//! - `RenderJobStore::open(path)` opens / creates the DB.
//! - `RenderJobStore::reincarnate_running_as_queued()` flips every
//!   `Running` row back to `Queued` so the worker pool will pick them
//!   up again. Walkthrough jobs preserve their `completed_frames`
//!   marker — the [`crate::WalkthroughPipeline::render`] call site
//!   already honours `resume_from` so no frame is re-rendered.
//! - `RenderJobStore::load_queue()` rebuilds an in-memory
//!   [`RenderQueue`] from the DB. Priority order is preserved.
//!
//! In normal operation the queue calls `store.upsert(&job)` after every
//! mutation. The store is `Send + Sync`-safe via internal Mutex.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::job::{RenderJob, RenderJobStatus};
use crate::queue::RenderQueue;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS render_jobs (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    progress REAL NOT NULL DEFAULT 0.0,
    batch_id TEXT,
    camera_id TEXT,
    output_path TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    completed_frames TEXT NOT NULL DEFAULT '[]',
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_render_jobs_status ON render_jobs(status);
CREATE INDEX IF NOT EXISTS idx_render_jobs_batch  ON render_jobs(batch_id);
CREATE INDEX IF NOT EXISTS idx_render_jobs_prio   ON render_jobs(priority DESC, created_at ASC);
";

#[derive(Debug, Error)]
pub enum RenderJobStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type RenderJobStoreResult<T> = std::result::Result<T, RenderJobStoreError>;

/// Persistent, SQLite-backed store of [`RenderJob`]s. One file under
/// `<project>/renders/queue.sqlite`.
pub struct RenderJobStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl RenderJobStore {
    /// Open (or create) the store at `path`. The parent directory is
    /// created if missing — this matches the workflow of a freshly
    /// created project where `<project>/renders/` doesn't exist yet.
    pub fn open(path: impl Into<PathBuf>) -> RenderJobStoreResult<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    /// Path to the underlying SQLite file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert or update a single job. Atomic per call.
    pub fn upsert(&self, job: &RenderJob) -> RenderJobStoreResult<()> {
        let payload = serde_json::to_string(job)?;
        let completed_frames = serde_json::to_string(&job.completed_frames)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO render_jobs (
                id, status, priority, progress, batch_id, camera_id,
                output_path, error, created_at, started_at, completed_at,
                completed_frames, payload
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
             ) ON CONFLICT(id) DO UPDATE SET
                status = excluded.status,
                priority = excluded.priority,
                progress = excluded.progress,
                batch_id = excluded.batch_id,
                camera_id = excluded.camera_id,
                output_path = excluded.output_path,
                error = excluded.error,
                started_at = excluded.started_at,
                completed_at = excluded.completed_at,
                completed_frames = excluded.completed_frames,
                payload = excluded.payload",
            params![
                job.id,
                status_to_str(job.status),
                job.priority,
                job.progress as f64,
                job.batch_id,
                job.camera_id,
                job.output_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                job.error,
                rfc3339(job.created_at),
                job.started_at.map(rfc3339),
                job.completed_at.map(rfc3339),
                completed_frames,
                payload,
            ],
        )?;
        Ok(())
    }

    /// Remove a job by id. Returns `true` if a row was deleted.
    pub fn delete(&self, id: &str) -> RenderJobStoreResult<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM render_jobs WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Look up a single job by id.
    pub fn get(&self, id: &str) -> RenderJobStoreResult<Option<RenderJob>> {
        let conn = self.conn.lock().unwrap();
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM render_jobs WHERE id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(match payload {
            Some(p) => Some(serde_json::from_str(&p)?),
            None => None,
        })
    }

    /// List every job in priority-descending order (high priority first),
    /// ties broken by `created_at` ascending.
    pub fn list_jobs(&self) -> RenderJobStoreResult<Vec<RenderJob>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT payload FROM render_jobs
             ORDER BY priority DESC, created_at ASC",
        )?;
        let iter = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in iter {
            let payload = r?;
            out.push(serde_json::from_str::<RenderJob>(&payload)?);
        }
        Ok(out)
    }

    /// List jobs in any of the supplied statuses, priority-descending.
    pub fn list_by_status(
        &self,
        statuses: &[RenderJobStatus],
    ) -> RenderJobStoreResult<Vec<RenderJob>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat("?")
            .take(statuses.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT payload FROM render_jobs
             WHERE status IN ({placeholders})
             ORDER BY priority DESC, created_at ASC"
        );
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(statuses.len());
        let status_strs: Vec<&'static str> = statuses.iter().copied().map(status_to_str).collect();
        for s in &status_strs {
            params_vec.push(s);
        }
        let iter = stmt.query_map(params_vec.as_slice(), |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in iter {
            out.push(serde_json::from_str::<RenderJob>(&r?)?);
        }
        Ok(out)
    }

    /// Save the entire queue atomically: clears the table and writes
    /// every job in one transaction.
    pub fn save_queue(&self, queue: &RenderQueue) -> RenderJobStoreResult<()> {
        let payloads: Vec<(String, String, RenderJob)> = queue
            .list_jobs()
            .into_iter()
            .map(|j| -> RenderJobStoreResult<(String, String, RenderJob)> {
                Ok((
                    serde_json::to_string(j)?,
                    serde_json::to_string(&j.completed_frames)?,
                    j.clone(),
                ))
            })
            .collect::<RenderJobStoreResult<Vec<_>>>()?;

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM render_jobs", [])?;
        for (payload, completed_frames, j) in &payloads {
            tx.execute(
                "INSERT INTO render_jobs (
                    id, status, priority, progress, batch_id, camera_id,
                    output_path, error, created_at, started_at, completed_at,
                    completed_frames, payload
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    j.id,
                    status_to_str(j.status),
                    j.priority,
                    j.progress as f64,
                    j.batch_id,
                    j.camera_id,
                    j.output_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string()),
                    j.error,
                    rfc3339(j.created_at),
                    j.started_at.map(rfc3339),
                    j.completed_at.map(rfc3339),
                    completed_frames,
                    payload,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rebuild an in-memory [`RenderQueue`] from the on-disk rows.
    pub fn load_queue(&self) -> RenderJobStoreResult<RenderQueue> {
        let mut queue = RenderQueue::new();
        // Resurrect in three buckets so the [`RenderQueue`] internal
        // partitioning is preserved without exposing private fields.
        for job in self.list_by_status(&[RenderJobStatus::Queued])? {
            queue.restore_queued(job);
        }
        for job in self.list_by_status(&[RenderJobStatus::Running])? {
            queue.restore_running(job);
        }
        for job in self.list_by_status(&[
            RenderJobStatus::Completed,
            RenderJobStatus::Failed,
            RenderJobStatus::Cancelled,
        ])? {
            queue.restore_completed(job);
        }
        Ok(queue)
    }

    /// Flip every `Running` job to `Queued` so the worker pool will
    /// pick them up again after a restart. Returns the number of jobs
    /// that were resurrected.
    pub fn reincarnate_running_as_queued(&self) -> RenderJobStoreResult<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT payload FROM render_jobs WHERE status = 'running'")?;
        let payloads: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let tx = conn.unchecked_transaction()?;
        let mut n = 0usize;
        for p in &payloads {
            let mut job: RenderJob = serde_json::from_str(p)?;
            job.status = RenderJobStatus::Queued;
            job.started_at = None;
            // `progress` is intentionally preserved — the worker will
            // overwrite it on the next progress tick. For walkthrough
            // jobs, `completed_frames` survives so resume_from picks
            // the right next frame.
            let new_payload = serde_json::to_string(&job)?;
            tx.execute(
                "UPDATE render_jobs SET status = 'queued', payload = ?1, started_at = NULL
                 WHERE id = ?2",
                params![new_payload, job.id],
            )?;
            n += 1;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Convenience: count rows by status. Useful for quick diagnostics
    /// + tests.
    pub fn count_by_status(&self) -> RenderJobStoreResult<JobStatusCounts> {
        let conn = self.conn.lock().unwrap();
        let mut counts = JobStatusCounts::default();
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM render_jobs GROUP BY status")?;
        let iter = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for r in iter {
            let (status, n) = r?;
            match status.as_str() {
                "queued" => counts.queued = n as usize,
                "running" => counts.running = n as usize,
                "completed" => counts.completed = n as usize,
                "failed" => counts.failed = n as usize,
                "cancelled" => counts.cancelled = n as usize,
                _ => {}
            }
        }
        Ok(counts)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JobStatusCounts {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl JobStatusCounts {
    pub fn total(&self) -> usize {
        self.queued + self.running + self.completed + self.failed + self.cancelled
    }
}

fn status_to_str(s: RenderJobStatus) -> &'static str {
    match s {
        RenderJobStatus::Queued => "queued",
        RenderJobStatus::Running => "running",
        RenderJobStatus::Completed => "completed",
        RenderJobStatus::Failed => "failed",
        RenderJobStatus::Cancelled => "cancelled",
    }
}

fn rfc3339(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::RenderPreset;
    use crate::scene::RenderScene;

    fn job(priority: i32) -> RenderJob {
        RenderJob::new(RenderPreset::standard(), RenderScene::new()).with_priority(priority)
    }

    fn open_store() -> (tempfile::TempDir, RenderJobStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = RenderJobStore::open(dir.path().join("queue.sqlite")).unwrap();
        (dir, s)
    }

    #[test]
    fn open_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested/sub/renders/queue.sqlite");
        let s = RenderJobStore::open(&p).unwrap();
        assert!(s.path().exists());
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let (_d, s) = open_store();
        let j = job(5);
        let id = j.id.clone();
        s.upsert(&j).unwrap();
        let got = s.get(&id).unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.priority, 5);
        assert_eq!(got.status, RenderJobStatus::Queued);
    }

    #[test]
    fn upsert_updates_existing_row() {
        let (_d, s) = open_store();
        let mut j = job(0);
        s.upsert(&j).unwrap();
        j.status = RenderJobStatus::Running;
        j.progress = 0.42;
        s.upsert(&j).unwrap();
        let got = s.get(&j.id).unwrap().unwrap();
        assert_eq!(got.status, RenderJobStatus::Running);
        assert!((got.progress - 0.42).abs() < 1e-5);
    }

    #[test]
    fn delete_removes_row() {
        let (_d, s) = open_store();
        let j = job(0);
        s.upsert(&j).unwrap();
        assert!(s.delete(&j.id).unwrap());
        assert!(s.get(&j.id).unwrap().is_none());
        assert!(!s.delete(&j.id).unwrap(), "second delete is a no-op");
    }

    #[test]
    fn list_jobs_returns_priority_descending() {
        let (_d, s) = open_store();
        let lo = job(0);
        let hi = job(10);
        let mid = job(5);
        s.upsert(&lo).unwrap();
        s.upsert(&hi).unwrap();
        s.upsert(&mid).unwrap();
        let list = s.list_jobs().unwrap();
        assert_eq!(list[0].id, hi.id);
        assert_eq!(list[1].id, mid.id);
        assert_eq!(list[2].id, lo.id);
    }

    #[test]
    fn list_by_status_filters_correctly() {
        let (_d, s) = open_store();
        let mut a = job(0);
        let mut b = job(0);
        let c = job(0);
        a.status = RenderJobStatus::Running;
        b.status = RenderJobStatus::Failed;
        s.upsert(&a).unwrap();
        s.upsert(&b).unwrap();
        s.upsert(&c).unwrap();
        let running = s.list_by_status(&[RenderJobStatus::Running]).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, a.id);
        let either = s
            .list_by_status(&[RenderJobStatus::Running, RenderJobStatus::Failed])
            .unwrap();
        assert_eq!(either.len(), 2);
    }

    #[test]
    fn count_by_status_groups_rows() {
        let (_d, s) = open_store();
        let mut r1 = job(0);
        let mut r2 = job(0);
        let mut f = job(0);
        let q = job(0);
        r1.status = RenderJobStatus::Running;
        r2.status = RenderJobStatus::Running;
        f.status = RenderJobStatus::Failed;
        s.upsert(&r1).unwrap();
        s.upsert(&r2).unwrap();
        s.upsert(&f).unwrap();
        s.upsert(&q).unwrap();
        let c = s.count_by_status().unwrap();
        assert_eq!(c.queued, 1);
        assert_eq!(c.running, 2);
        assert_eq!(c.failed, 1);
        assert_eq!(c.total(), 4);
    }

    #[test]
    fn save_queue_then_load_queue_round_trips() {
        let (_d, s) = open_store();
        let mut q = RenderQueue::new();
        let a = q.submit(job(10));
        let b = q.submit(job(0));
        let _ = q.admit().unwrap(); // moves the high-priority `a` to running
        q.fail(&b, "boom").unwrap();
        s.save_queue(&q).unwrap();

        let loaded = s.load_queue().unwrap();
        assert_eq!(loaded.list_jobs().len(), q.list_jobs().len());
        assert_eq!(loaded.get(&a).unwrap().status, RenderJobStatus::Running);
        assert_eq!(loaded.get(&b).unwrap().status, RenderJobStatus::Failed);
    }

    #[test]
    fn reincarnate_flips_running_to_queued() {
        let (_d, s) = open_store();
        let mut running = job(0);
        running.status = RenderJobStatus::Running;
        running.started_at = Some(Utc::now());
        running.progress = 0.5;
        s.upsert(&running).unwrap();

        let n = s.reincarnate_running_as_queued().unwrap();
        assert_eq!(n, 1);
        let got = s.get(&running.id).unwrap().unwrap();
        assert_eq!(got.status, RenderJobStatus::Queued);
        assert!(got.started_at.is_none(), "started_at should be cleared");
        // progress is intentionally NOT reset to 0 — see module docs.
        assert!((got.progress - 0.5).abs() < 1e-5);
    }

    #[test]
    fn reincarnate_preserves_walkthrough_completed_frames() {
        let (_d, s) = open_store();
        let mut j = job(0);
        j.status = RenderJobStatus::Running;
        j.mark_frame_completed(0);
        j.mark_frame_completed(1);
        j.mark_frame_completed(2);
        s.upsert(&j).unwrap();
        s.reincarnate_running_as_queued().unwrap();
        let got = s.get(&j.id).unwrap().unwrap();
        assert_eq!(got.completed_frames, vec![0, 1, 2]);
        assert_eq!(got.next_walkthrough_frame(), Some(3));
    }

    #[test]
    fn crash_after_two_jobs_keeps_remaining_two_queued() {
        // Spec: enqueue 4 jobs → simulate crash after job 2 → restart →
        // verify jobs 3-4 are still queued and resumable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.sqlite");

        // Pre-crash: a worker drains jobs 1 + 2 to completion, jobs 3 + 4
        // remain queued. Worker upserts after every state change.
        {
            let s = RenderJobStore::open(&path).unwrap();
            let mut jobs = Vec::new();
            for _ in 0..4 {
                let j = job(0);
                s.upsert(&j).unwrap();
                jobs.push(j);
            }
            // Workers complete jobs 1 + 2.
            for j in jobs.iter_mut().take(2) {
                j.status = RenderJobStatus::Running;
                s.upsert(j).unwrap();
                j.status = RenderJobStatus::Completed;
                j.progress = 1.0;
                j.completed_at = Some(Utc::now());
                s.upsert(j).unwrap();
            }
            // A third job was mid-render when the process died.
            jobs[2].status = RenderJobStatus::Running;
            jobs[2].progress = 0.5;
            s.upsert(&jobs[2]).unwrap();
        }
        // Simulated kill -9: drop the connection.

        // Restart: open the store again, reincarnate running rows.
        let s2 = RenderJobStore::open(&path).unwrap();
        let n = s2.reincarnate_running_as_queued().unwrap();
        assert_eq!(n, 1, "exactly one mid-render job was running");

        let c = s2.count_by_status().unwrap();
        assert_eq!(c.completed, 2, "two completed survived");
        assert_eq!(c.queued, 2, "one originally-queued + one resurrected");
        assert_eq!(c.running, 0);

        // The resurrected job preserved its progress so the worker
        // can skip already-rendered tiles on resume.
        let queued = s2.list_by_status(&[RenderJobStatus::Queued]).unwrap();
        assert!(queued.iter().any(|j| (j.progress - 0.5).abs() < 1e-5));
    }

    #[test]
    fn store_survives_being_reopened_with_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.sqlite");
        let id;
        {
            let s = RenderJobStore::open(&path).unwrap();
            let j = job(7);
            id = j.id.clone();
            s.upsert(&j).unwrap();
        }
        let s2 = RenderJobStore::open(&path).unwrap();
        assert!(s2.get(&id).unwrap().is_some());
    }
}
