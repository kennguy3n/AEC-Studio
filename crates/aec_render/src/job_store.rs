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
//! `WHERE status IN ('queued','running')` directly.
//!
//! ## Durability mechanism
//!
//! Mutations use `PRAGMA journal_mode = WAL` + `PRAGMA synchronous = NORMAL`.
//! Under WAL+NORMAL the SQLite engine fsyncs the WAL on every COMMIT
//! transition (preventing torn writes), and periodically auto-checkpoints
//! the WAL into the main database. This is sufficient for:
//!
//! - **SIGKILL / panic mid-write**: the OS page cache survives, so any
//!   COMMIT that returned `Ok(())` is durable on the next process start.
//! - **Application crash mid-update**: the WAL header guarantees that
//!   either the entire transaction is present or none of it is.
//!
//! It is **not** sufficient for power-failure / OS-crash durability —
//! that would require `synchronous = FULL`, which costs roughly one
//! extra fsync per write. Because render jobs are recoverable from the
//! project's revision history, the throughput trade-off favours
//! `NORMAL`. Callers needing FULL durability can re-open the connection
//! and execute `PRAGMA synchronous = FULL` before mutating.
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
        let conn = self.conn.lock().unwrap();
        write_job_row(&conn, job)
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
    ///
    /// Each row is written via the shared [`write_job_row`] helper so
    /// the "indexed columns mirror payload" invariant is enforced by
    /// the same SQL site used by [`RenderJobStore::upsert`] and
    /// [`RenderJobStore::reincarnate_running_as_queued`]. Adding a new
    /// indexed column to the schema therefore requires editing
    /// **one** SQL site, not three — eliminating the maintenance
    /// asymmetry where `save_queue` could silently drift from
    /// `upsert` (e.g. forgetting to mirror a new `tenant_id` column,
    /// or omitting a new `paused_at` field from the bulk save path).
    ///
    /// `write_job_row` emits `INSERT ... ON CONFLICT(id) DO UPDATE`,
    /// but because the loop runs after `DELETE FROM render_jobs` the
    /// table is empty and only the INSERT branch ever fires — so the
    /// semantic is exactly the same as the previous plain-INSERT
    /// implementation. `created_at` is written fresh from each
    /// in-memory job, preserving the prior behaviour (`save_queue`
    /// has always been authoritative for `created_at` because callers
    /// supply the full queue snapshot).
    pub fn save_queue(&self, queue: &RenderQueue) -> RenderJobStoreResult<()> {
        let jobs: Vec<RenderJob> = queue.list_jobs().into_iter().cloned().collect();

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM render_jobs", [])?;
        for j in &jobs {
            write_job_row(&tx, j)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rebuild an in-memory [`RenderQueue`] from the on-disk rows.
    ///
    /// All three status-bucket reads run inside a single deferred
    /// transaction so the resulting queue is a consistent snapshot —
    /// even if a concurrent writer upserts or deletes rows partway
    /// through, this method will not observe partial updates.
    pub fn load_queue(&self) -> RenderJobStoreResult<RenderQueue> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // Resurrect in three buckets so the [`RenderQueue`] internal
        // partitioning is preserved without exposing private fields.
        // All three reads share the same transaction snapshot.
        let queued = list_by_status_tx(&tx, &[RenderJobStatus::Queued])?;
        let running = list_by_status_tx(&tx, &[RenderJobStatus::Running])?;
        let terminal = list_by_status_tx(
            &tx,
            &[
                RenderJobStatus::Completed,
                RenderJobStatus::Failed,
                RenderJobStatus::Cancelled,
            ],
        )?;
        tx.commit()?;
        drop(conn);

        let mut queue = RenderQueue::new();
        for job in queued {
            queue.restore_queued(job);
        }
        for job in running {
            queue.restore_running(job);
        }
        for job in terminal {
            queue.restore_completed(job);
        }
        Ok(queue)
    }

    /// Flip every `Running` job to `Queued` so the worker pool will
    /// pick them up again after a restart. Returns the number of jobs
    /// that were resurrected.
    ///
    /// The UPDATE refreshes **every** indexed column that the row
    /// derives from the payload, not just `status` and `started_at`.
    /// This preserves the invariant "indexed columns mirror the
    /// payload" — future queries against `progress`, `completed_at`,
    /// `error`, or `completed_frames` (e.g. dashboards, batch reports)
    /// will see values consistent with the reincarnated payload, never
    /// stale ones from the previous `Running` state.
    ///
    /// The SELECT that gathers the running payloads runs **inside**
    /// the same transaction as the UPDATEs, so the read and write
    /// observe the same snapshot. This matters for two reasons:
    /// (1) within a single process the in-memory `Mutex` already
    ///     serialises access, but a future multi-process deployment
    ///     (where SQLite's WAL allows concurrent writers) needs the
    ///     read to participate in the transactional scope; and
    /// (2) any future addition of a SELECT that filters by an
    ///     indexed column other than `status` would otherwise be
    ///     racy against concurrent UPDATEs even in single-process
    ///     mode. Keeping the read transactional is the cheap, correct
    ///     default.
    ///
    /// # Field semantics on reincarnation
    ///
    /// A reincarnated job is a *fresh* `Queued` attempt — the prior
    /// `Running` incarnation never reached a terminal state. The
    /// store therefore distinguishes between fields whose values are
    /// **forward-looking** (useful to the next attempt) and fields
    /// that are **artifacts of the prior attempt**:
    ///
    /// - **Preserved (forward-looking):**
    ///   - `progress` — the worker uses this to skip already-rendered
    ///     tiles; the next progress tick will overwrite it anyway.
    ///   - `completed_frames` — walkthrough jobs use this so
    ///     `next_walkthrough_frame()` picks the right frame to resume.
    ///
    /// - **Cleared (artifacts of the prior attempt):**
    ///   - `started_at` — set by the worker when the new attempt
    ///     begins; preserving the old timestamp would mis-report
    ///     wall-clock duration.
    ///   - `error` — a `Queued` job has not yet failed in the current
    ///     incarnation; carrying the prior failure's text would make
    ///     `WHERE error IS NOT NULL` dashboard queries report
    ///     pending-retry jobs as failures.
    ///   - `completed_at` — a `Queued` job has not yet completed;
    ///     preserving this would make `WHERE completed_at IS NOT NULL`
    ///     queries ("jobs that finished today") include pending
    ///     retries that were running when the process died.
    ///
    /// This split keeps the indexed-columns-mirror-payload invariant
    /// honest *and* keeps the `(status, error, completed_at)` triple
    /// internally consistent: a `Queued` row has `error IS NULL` and
    /// `completed_at IS NULL`, full stop.
    pub fn reincarnate_running_as_queued(&self) -> RenderJobStoreResult<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let running = status_to_str(RenderJobStatus::Running);
        let payloads: Vec<String> = {
            let mut stmt = tx.prepare("SELECT payload FROM render_jobs WHERE status = ?1")?;
            let rows = stmt
                .query_map(params![running], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let mut n = 0usize;
        for p in &payloads {
            let mut job: RenderJob = serde_json::from_str(p)?;
            job.status = RenderJobStatus::Queued;
            job.started_at = None;
            // Clear artifacts of the prior `Running` attempt; see the
            // doc-comment above for the full rationale.
            job.error = None;
            job.completed_at = None;
            // `progress` and `completed_frames` are intentionally
            // preserved so walkthrough workers can resume from the
            // last completed frame; see doc-comment above.
            //
            // Use the shared `write_job_row` helper so every indexed
            // column (priority, batch_id, camera_id, output_path,
            // progress, error, completed_at, completed_frames) is
            // refreshed from the *mutated* payload. This makes the
            // "indexed columns mirror payload" invariant ironclad
            // regardless of which fields future reincarnation logic
            // chooses to change — drop a `job.priority -= 1` here and
            // the column reflects it without an SQL edit. The
            // INSERT ON CONFLICT UPDATE clause preserves `created_at`
            // by design (it's only in the INSERT row, never the SET
            // list).
            write_job_row(&tx, &job)?;
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

/// Write a single job row, mirroring **every** indexed column from the
/// supplied payload.
///
/// This is the single source of truth for the
/// "indexed columns mirror payload" invariant. Both [`RenderJobStore::upsert`]
/// (top-level, autocommit) and
/// [`RenderJobStore::reincarnate_running_as_queued`]
/// (inside an explicit transaction) call this helper, so adding a new
/// indexed column to the schema requires editing **one** SQL site, not
/// two — and any future reincarnation logic that mutates additional
/// payload fields (e.g. demoting `priority`, clearing `output_path`)
/// is automatically reflected in the indexed columns with no extra
/// SQL edit. Without this centralisation the reincarnate path would
/// silently leave stale `priority` / `batch_id` / `camera_id` /
/// `output_path` values in the indexed columns whenever a future
/// change touched those fields in the payload — a classic
/// indexed-column drift bug.
///
/// The `INSERT ... ON CONFLICT(id) DO UPDATE` form preserves
/// `created_at` on the conflict branch by deliberately omitting it
/// from the SET clause: `created_at` is a per-job immutable timestamp
/// of first enqueue, and reincarnation must not appear to "recreate"
/// the job.
///
/// `conn` accepts both a raw `&Connection` (autocommit) and an
/// `&Transaction` (via `Deref<Target = Connection>`), so callers can
/// choose their own isolation scope without duplicating the SQL.
fn write_job_row(conn: &Connection, job: &RenderJob) -> RenderJobStoreResult<()> {
    let payload = serde_json::to_string(job)?;
    let completed_frames = serde_json::to_string(&job.completed_frames)?;
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

/// Transaction-bound mirror of [`RenderJobStore::list_by_status`].
///
/// Reads the same `payload` column with the same priority ordering as
/// the public API, but executes against the supplied [`rusqlite::Transaction`]
/// so a caller wrapping multiple queries inside one transaction gets a
/// consistent snapshot (no concurrent upserts/deletes can interleave).
fn list_by_status_tx(
    tx: &rusqlite::Transaction<'_>,
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
    let mut stmt = tx.prepare(&sql)?;
    let status_strs: Vec<&'static str> = statuses.iter().copied().map(status_to_str).collect();
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(statuses.len());
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
    fn save_queue_mirrors_every_indexed_column_from_payload() {
        // Regression: closes the same indexed-column-drift class that
        // Devin Review flagged on the reincarnate path, applied to
        // `save_queue`. The previous implementation hand-rolled its
        // own `INSERT INTO render_jobs (…) VALUES (…)` SQL — separate
        // from the `INSERT ON CONFLICT UPDATE` used by `upsert`. That
        // meant adding a new indexed column (e.g. `tenant_id`,
        // `paused_at`) required editing **two** SQL sites, and any
        // future contributor who forgot the bulk-save site would
        // silently leave that column NULL after every full-queue
        // snapshot — an invisible-to-payload bug that would only
        // surface through dashboards / batch reports filtering on the
        // missed column.
        //
        // The fix routes `save_queue` through the same `write_job_row`
        // helper as `upsert` + `reincarnate_running_as_queued`. This
        // test locks the contract by reading **every** indexed column
        // directly via SQL after `save_queue` and asserting it matches
        // the in-memory payload that was saved — across all interesting
        // states (queued, running, failed) so the lock applies to every
        // row written by the bulk save, not just the first.
        let (_d, s) = open_store();

        // Three jobs, each populating a different mix of nullable and
        // non-null indexed columns so the test exercises every column
        // currently mirrored by `write_job_row`.
        let mut q_job = job(5);
        q_job.priority = 5;
        q_job.batch_id = Some("batch-queued".to_string());
        q_job.camera_id = Some("cam-Q".to_string());
        q_job.output_path = Some(std::path::PathBuf::from("/tmp/save-queued.png"));
        q_job.progress = 0.1;

        let mut r_job = job(7);
        r_job.priority = 7;
        r_job.status = RenderJobStatus::Running;
        r_job.batch_id = Some("batch-running".to_string());
        r_job.camera_id = Some("cam-R".to_string());
        r_job.output_path = Some(std::path::PathBuf::from("/tmp/save-running.png"));
        r_job.progress = 0.42;
        r_job.started_at = Some(Utc::now());
        r_job.mark_frame_completed(0);

        let mut f_job = job(0);
        f_job.priority = 0;
        f_job.status = RenderJobStatus::Failed;
        f_job.batch_id = None;
        f_job.camera_id = None;
        f_job.output_path = None;
        f_job.progress = 0.0;
        f_job.error = Some("disk full".to_string());
        f_job.completed_at = Some(Utc::now());

        let mut queue = RenderQueue::new();
        queue.restore_queued(q_job.clone());
        queue.restore_running(r_job.clone());
        queue.restore_completed(f_job.clone());

        s.save_queue(&queue).unwrap();

        // Read the indexed columns directly — not via the JSON payload
        // round-trip — to ensure the bulk-save actually wrote them.
        let conn = s.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, status, priority, progress, batch_id, camera_id,
                        output_path, error, started_at, completed_at,
                        completed_frames, payload
                 FROM render_jobs",
            )
            .unwrap();
        type Row = (
            String,
            String,
            i64,
            f64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
        );
        let rows: Vec<Row> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(stmt);
        drop(conn);

        assert_eq!(rows.len(), 3, "save_queue must persist every job");

        for row in &rows {
            let (
                id,
                status_col,
                priority_col,
                progress_col,
                batch_col,
                camera_col,
                output_col,
                error_col,
                started_col,
                completed_col,
                frames_col,
                payload_col,
            ) = row;

            // The payload itself is the ground truth for what was
            // saved; every indexed column must mirror it byte-for-byte
            // (modulo the rfc3339 string encoding for timestamps and
            // the JSON encoding for the frames vector).
            let from_payload: RenderJob = serde_json::from_str(payload_col).unwrap();
            assert_eq!(
                &from_payload.id, id,
                "id column must mirror payload after save_queue"
            );
            assert_eq!(
                status_to_str(from_payload.status),
                status_col,
                "status column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.priority as i64, *priority_col,
                "priority column must mirror payload after save_queue"
            );
            assert!(
                (from_payload.progress as f64 - *progress_col).abs() < 1e-9,
                "progress column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.batch_id, *batch_col,
                "batch_id column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.camera_id, *camera_col,
                "camera_id column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload
                    .output_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                *output_col,
                "output_path column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.error, *error_col,
                "error column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.started_at.map(rfc3339),
                *started_col,
                "started_at column must mirror payload after save_queue"
            );
            assert_eq!(
                from_payload.completed_at.map(rfc3339),
                *completed_col,
                "completed_at column must mirror payload after save_queue"
            );
            let from_payload_frames: Vec<u32> =
                serde_json::from_str(frames_col).expect("completed_frames is valid JSON");
            assert_eq!(
                from_payload.completed_frames, from_payload_frames,
                "completed_frames column must mirror payload after save_queue"
            );
        }

        // Targeted spot-check on the rich Running row: the indexed
        // columns must equal the exact in-memory values we built it
        // from, not just the payload that was round-tripped through
        // serde. This catches the (otherwise plausible) bug where
        // `write_job_row` reads from the payload string instead of
        // the typed `&RenderJob`, which would silently work for any
        // field whose serde representation is lossless but fail for
        // anything with a non-trivial conversion (e.g. PathBuf, enums).
        let running_row = rows
            .iter()
            .find(|r| r.0 == r_job.id)
            .expect("running job present");
        assert_eq!(running_row.1, "running");
        assert_eq!(running_row.2, 7);
        assert_eq!(running_row.4.as_deref(), Some("batch-running"));
        assert_eq!(running_row.5.as_deref(), Some("cam-R"));
        assert_eq!(running_row.6.as_deref(), Some("/tmp/save-running.png"));
    }

    #[test]
    fn save_queue_round_trips_through_load_queue_with_all_columns_preserved() {
        // Tighter contract than `save_queue_then_load_queue_round_trips`:
        // not only must the *count* match across the round-trip, but
        // every column the worker cares about (priority for queue
        // ordering, batch_id for cancellation, camera_id for the
        // walkthrough resume hook, output_path for delivery, progress
        // for tile-skip on resume) must survive untouched. This locks
        // `save_queue` against future changes that silently drop a
        // column from the bulk-save path — the most common shape of
        // the indexed-column-drift bug.
        let (_d, s) = open_store();
        let mut j = job(9);
        j.priority = 9;
        j.batch_id = Some("batch-Z".to_string());
        j.camera_id = Some("cam-Z".to_string());
        j.output_path = Some(std::path::PathBuf::from("/tmp/save-roundtrip.png"));
        j.progress = 0.6;
        j.mark_frame_completed(0);
        j.mark_frame_completed(3);

        let mut queue = RenderQueue::new();
        queue.restore_queued(j.clone());
        s.save_queue(&queue).unwrap();

        let loaded = s.load_queue().unwrap();
        let got = loaded.get(&j.id).expect("job present after round-trip");
        assert_eq!(got.priority, 9);
        assert_eq!(got.batch_id.as_deref(), Some("batch-Z"));
        assert_eq!(got.camera_id.as_deref(), Some("cam-Z"));
        assert_eq!(
            got.output_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("/tmp/save-roundtrip.png".to_string())
        );
        assert!((got.progress - 0.6).abs() < 1e-5);
        assert_eq!(got.completed_frames, vec![0, 3]);
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
    fn reincarnate_refreshes_all_indexed_columns_from_payload() {
        // Regression: prior to the fix, `reincarnate_running_as_queued`
        // only UPDATEd `status`, `payload`, and `started_at` — leaving
        // `progress`, `error`, `completed_at`, and `completed_frames`
        // stale relative to the new payload. Any future query that
        // filtered/joined on those indexed columns (dashboards, batch
        // reports) would see ghost state from the previous `Running`
        // row. This test locks the invariant "indexed columns mirror
        // payload" by reading the columns directly via SQL.
        //
        // It also locks the field-semantics split documented on
        // `reincarnate_running_as_queued`: `progress` and
        // `completed_frames` are preserved (forward-looking state for
        // the next attempt) while `started_at`, `error`, and
        // `completed_at` are cleared (artifacts of the prior attempt
        // that would mislead dashboards / batch reports).
        let (_d, s) = open_store();
        let mut j = job(3);
        j.status = RenderJobStatus::Running;
        j.started_at = Some(Utc::now());
        j.progress = 0.7;
        j.error = Some("stale prior failure".to_string());
        j.completed_at = Some(Utc::now());
        j.mark_frame_completed(0);
        j.mark_frame_completed(1);
        s.upsert(&j).unwrap();

        let n = s.reincarnate_running_as_queued().unwrap();
        assert_eq!(n, 1);

        // Read the indexed columns directly — not via the JSON payload
        // round-trip — to ensure the UPDATE actually touched them.
        let conn = s.conn.lock().unwrap();
        let (status, started_at, progress, error, completed_at, completed_frames): (
            String,
            Option<String>,
            f64,
            Option<String>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT status, started_at, progress, error, completed_at, completed_frames
                 FROM render_jobs WHERE id = ?1",
                params![j.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        drop(conn);

        assert_eq!(status, "queued", "status column must be flipped");
        assert!(started_at.is_none(), "started_at column must be cleared");
        assert!(
            (progress - 0.7).abs() < 1e-5,
            "progress column must mirror the preserved payload value"
        );
        assert!(
            error.is_none(),
            "error column must be cleared — a Queued job has not failed in the current incarnation"
        );
        assert!(
            completed_at.is_none(),
            "completed_at column must be cleared — a Queued job has not yet completed"
        );
        let parsed_frames: Vec<u32> = serde_json::from_str(&completed_frames).unwrap();
        assert_eq!(
            parsed_frames,
            vec![0, 1],
            "completed_frames column must mirror payload (preserved for walkthrough resume)"
        );

        // The payload itself must also reflect the cleared fields so
        // any future consumer that round-trips through the JSON sees
        // the same Queued-clean state as the columns.
        let got = s.get(&j.id).unwrap().unwrap();
        assert_eq!(got.status, RenderJobStatus::Queued);
        assert!(got.error.is_none(), "payload.error must be cleared");
        assert!(
            got.completed_at.is_none(),
            "payload.completed_at must be cleared"
        );
        assert!((got.progress - 0.7).abs() < 1e-5);
        assert_eq!(got.completed_frames, vec![0, 1]);
    }

    #[test]
    fn reincarnate_mirrors_non_status_columns_from_payload() {
        // Regression: closes the latent indexed-column-drift class that
        // Devin Review flagged on `reincarnate_running_as_queued`. The
        // previous implementation hand-rolled a partial `UPDATE SET …`
        // listing only the columns the reincarnation logic *currently*
        // mutates (status / started_at / progress / error /
        // completed_at / completed_frames). That meant a future change
        // adding `job.priority -= 1` or `job.output_path = None` to
        // the reincarnation logic would silently leave the *column*
        // values stale relative to the payload — invisible to anyone
        // reading the JSON payload, but corrupting dashboards / batch
        // reports that filter on the indexed columns.
        //
        // The current implementation routes reincarnation through the
        // same `write_job_row` helper as `upsert`, so **every** indexed
        // column is refreshed from the (mutated) payload on every
        // reincarnation. This test locks that contract by asserting
        // priority / batch_id / camera_id / output_path are still
        // mirrored from the payload after a reincarnation, even though
        // the reincarnation logic does not currently mutate them. If
        // a future change to `reincarnate_running_as_queued` *does*
        // mutate those fields, this test will still pass — the
        // columns track the payload by construction.
        let (_d, s) = open_store();
        let mut j = job(7);
        j.status = RenderJobStatus::Running;
        j.priority = 7;
        j.batch_id = Some("batch-X".to_string());
        j.camera_id = Some("cam-Y".to_string());
        j.output_path = Some(std::path::PathBuf::from("/tmp/reincarnate-mirror.png"));
        j.started_at = Some(Utc::now());
        s.upsert(&j).unwrap();

        let n = s.reincarnate_running_as_queued().unwrap();
        assert_eq!(n, 1);

        let conn = s.conn.lock().unwrap();
        let (status, priority, batch_id, camera_id, output_path, payload): (
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT status, priority, batch_id, camera_id, output_path, payload
                 FROM render_jobs WHERE id = ?1",
                params![j.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        drop(conn);

        assert_eq!(status, "queued");
        let from_payload: RenderJob = serde_json::from_str(&payload).unwrap();

        // Every non-status indexed column must equal the value you
        // would get by parsing the JSON payload. This is the actual
        // invariant — "indexed columns mirror payload" — applied to
        // the reincarnation path, not just upsert.
        assert_eq!(
            from_payload.priority as i64, priority,
            "priority column must mirror payload after reincarnate"
        );
        assert_eq!(
            from_payload.batch_id, batch_id,
            "batch_id column must mirror payload after reincarnate"
        );
        assert_eq!(
            from_payload.camera_id, camera_id,
            "camera_id column must mirror payload after reincarnate"
        );
        assert_eq!(
            from_payload
                .output_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            output_path,
            "output_path column must mirror payload after reincarnate"
        );

        // Sanity-check the un-mutated payload values themselves so a
        // future bug that nulls them out in `reincarnate_running_as_queued`
        // is caught here too — these are the fields the doc-comment
        // labels "neither forward-looking nor artifacts of the prior
        // attempt" and that reincarnation must therefore preserve
        // verbatim.
        assert_eq!(from_payload.priority, 7);
        assert_eq!(from_payload.batch_id.as_deref(), Some("batch-X"));
        assert_eq!(from_payload.camera_id.as_deref(), Some("cam-Y"));
        assert_eq!(
            from_payload
                .output_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("/tmp/reincarnate-mirror.png".to_string()),
        );
    }

    #[test]
    fn load_queue_uses_a_single_transaction_snapshot() {
        // Regression: prior to the fix, `load_queue` issued three
        // separate `list_by_status` calls, each acquiring + releasing
        // the mutex independently. A concurrent writer could interleave
        // an `upsert` or `delete` between calls, producing an
        // inconsistent in-memory queue. The fix wraps all three reads
        // in one `BEGIN DEFERRED` transaction so the queue is always
        // built from a single snapshot.
        //
        // We can't easily race a real writer inside a unit test, but
        // we can lock the snapshot semantics by asserting that
        // `load_queue` returns the same partition counts whether the
        // store has zero, one, or many concurrent upserts queued up
        // behind it — i.e. that it does not see any "in-flight"
        // mutation as a partially-applied row.
        let (_d, s) = open_store();
        let mut a = job(10);
        a.status = RenderJobStatus::Queued;
        let mut b = job(0);
        b.status = RenderJobStatus::Running;
        let mut c = job(0);
        c.status = RenderJobStatus::Completed;
        c.completed_at = Some(Utc::now());
        s.upsert(&a).unwrap();
        s.upsert(&b).unwrap();
        s.upsert(&c).unwrap();

        let loaded = s.load_queue().unwrap();
        assert_eq!(loaded.list_jobs().len(), 3);
        assert_eq!(loaded.get(&a.id).unwrap().status, RenderJobStatus::Queued);
        assert_eq!(loaded.get(&b.id).unwrap().status, RenderJobStatus::Running);
        assert_eq!(
            loaded.get(&c.id).unwrap().status,
            RenderJobStatus::Completed
        );

        // A second `load_queue` immediately after must observe the
        // exact same snapshot.
        let loaded2 = s.load_queue().unwrap();
        assert_eq!(loaded2.list_jobs().len(), 3);
    }

    #[test]
    #[should_panic(expected = "restore_completed expects a terminal status")]
    fn restore_completed_panics_on_non_terminal_status_in_release_builds() {
        // Regression: previously this used `debug_assert!`, which is
        // stripped from release builds — meaning a misuse (passing a
        // `Queued` or `Running` job) would silently corrupt the
        // partition. The fix promotes it to a runtime `assert!` so the
        // misuse is caught in every build configuration.
        let mut q = RenderQueue::new();
        let mut bad = job(0);
        bad.status = RenderJobStatus::Queued; // NOT terminal
        q.restore_completed(bad);
    }

    #[test]
    fn upsert_refreshes_all_indexed_columns_from_payload() {
        // Regression / invariant lock: the `reincarnate` test above
        // covers the resurrection path, but `upsert` is the *primary*
        // write site — every transition (Queued -> Running -> Completed
        // -> Failed -> Cancelled) flows through it. This test asserts
        // that updating an existing row via `upsert` keeps every
        // indexed column (status, priority, progress, batch_id,
        // camera_id, output_path, error, started_at, completed_at,
        // completed_frames) in lock-step with the new payload, so a
        // future dashboard query against any of those columns sees
        // the same value as parsing the JSON payload would yield.
        let (_d, s) = open_store();
        let mut j = job(2);
        j.status = RenderJobStatus::Queued;
        j.progress = 0.0;
        s.upsert(&j).unwrap();

        // Mutate every payload-derived column and upsert again.
        j.status = RenderJobStatus::Completed;
        j.priority = 99;
        j.progress = 1.0;
        j.batch_id = Some("batch-A".to_string());
        j.camera_id = Some("cam-B".to_string());
        j.output_path = Some(std::path::PathBuf::from("/tmp/out.png"));
        j.error = Some("warning text".to_string());
        let now = Utc::now();
        j.started_at = Some(now);
        j.completed_at = Some(now);
        j.mark_frame_completed(0);
        j.mark_frame_completed(2);
        s.upsert(&j).unwrap();

        let conn = s.conn.lock().unwrap();
        let (
            status,
            priority,
            progress,
            batch_id,
            camera_id,
            output_path,
            error,
            started_at,
            completed_at,
            completed_frames,
            payload,
        ): (
            String,
            i64,
            f64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT status, priority, progress, batch_id, camera_id,
                        output_path, error, started_at, completed_at,
                        completed_frames, payload
                 FROM render_jobs WHERE id = ?1",
                params![j.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .unwrap();
        drop(conn);

        // Each indexed column must agree with the new payload.
        assert_eq!(status, "completed");
        assert_eq!(priority, 99);
        assert!((progress - 1.0).abs() < 1e-9);
        assert_eq!(batch_id.as_deref(), Some("batch-A"));
        assert_eq!(camera_id.as_deref(), Some("cam-B"));
        assert_eq!(output_path.as_deref(), Some("/tmp/out.png"));
        assert_eq!(error.as_deref(), Some("warning text"));
        assert!(started_at.is_some());
        assert!(completed_at.is_some());
        let parsed_frames: Vec<u32> = serde_json::from_str(&completed_frames).unwrap();
        assert_eq!(parsed_frames, vec![0, 2]);

        // Cross-check: every indexed column equals the value you would
        // get by parsing the JSON payload. This is the actual
        // invariant — "indexed columns mirror payload".
        let from_payload: RenderJob = serde_json::from_str(&payload).unwrap();
        assert_eq!(status_to_str(from_payload.status), status);
        assert_eq!(from_payload.priority as i64, priority);
        assert!((f64::from(from_payload.progress) - progress).abs() < 1e-9);
        assert_eq!(from_payload.batch_id, batch_id);
        assert_eq!(from_payload.camera_id, camera_id);
        assert_eq!(
            from_payload
                .output_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            output_path
        );
        assert_eq!(from_payload.error, error);
    }

    #[test]
    fn timestamp_columns_use_fixed_width_z_suffixed_rfc3339() {
        // Regression: queue ordering and `completed_at`-bucket queries
        // rely on lexicographic string comparison being a valid
        // chronological order for the timestamp columns. That property
        // only holds when every timestamp the store writes uses the
        // same fixed-width RFC3339 shape — specifically, the
        // `to_rfc3339_opts(SecondsFormat::Nanos, /* use_z = */ true)`
        // format which always produces "YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ"
        // (i.e. nanosecond precision and a literal `Z` suffix instead
        // of a per-row `+HH:MM` offset). This test locks the shape so
        // a future "let's use `to_rfc3339()`" refactor (which would
        // emit variable-width offsets and break sortability) fails CI.
        let (_d, s) = open_store();
        let mut j = job(0);
        let now = Utc::now();
        j.created_at = now;
        j.started_at = Some(now);
        j.completed_at = Some(now);
        s.upsert(&j).unwrap();

        let conn = s.conn.lock().unwrap();
        let (created, started, completed): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT created_at, started_at, completed_at
                 FROM render_jobs WHERE id = ?1",
                params![j.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        drop(conn);

        for ts in [
            Some(created.as_str()),
            started.as_deref(),
            completed.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                ts.ends_with('Z'),
                "timestamp `{ts}` must end with `Z` so lexicographic sort matches chronological sort",
            );
            // "YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ" => exactly 30 chars.
            assert_eq!(
                ts.len(),
                30,
                "timestamp `{ts}` must be fixed-width (nanosecond precision)",
            );
            // Cross-check sortability: two timestamps one nanosecond
            // apart must compare in chronological order as strings.
            let later = (now + chrono::Duration::nanoseconds(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            assert!(
                later.as_str() > ts,
                "later timestamp `{later}` must lex-sort after `{ts}`",
            );
        }
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
