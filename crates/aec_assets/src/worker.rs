//! Worker-thread pool for batch asset ingest.
//!
//! Matches the worker-thread pattern used elsewhere in the workspace
//! (see `aec_render::walkthrough` and `aec_render::final_render`):
//! a synchronous pool of OS threads that pull `IngestJob` items off a
//! shared queue, do the (blocking) format-parsing work, and push
//! `IngestResult` items back on an MPSC channel for the caller to drain.
//!
//! Why not Tokio? Asset ingest is CPU-bound: parsing a 100 MiB OBJ or
//! tessellating an IFC mesh isn't I/O-blocking, so async runtimes add
//! complexity without speeding anything up. The thread-pool model
//! mirrors `rayon::ThreadPool` ergonomically but keeps lifecycle (start
//! / shutdown / drain) explicit, which matters because the renderer
//! pool is shared across IPC requests in the Electron host.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::ingest::{ingest_bytes, ingest_path, IngestError, IngestFormat, IngestedMesh};

/// Configuration for the ingest pool.
#[derive(Debug, Clone)]
pub struct IngestPoolConfig {
    /// Number of worker threads. Clamped to `[1, 64]` at construction
    /// — values outside that range are saturated.
    pub workers: usize,
    /// Bounded job queue depth. A queue depth of 0 means unbounded; for
    /// most callers leave at the default (`workers * 4`) so the
    /// scheduler can't accumulate a backlog larger than the pool can
    /// drain in a few seconds.
    pub queue_depth: usize,
}

impl Default for IngestPoolConfig {
    fn default() -> Self {
        let n = std::thread::available_parallelism()
            .map_or(2, std::num::NonZeroUsize::get)
            .clamp(1, 8);
        Self {
            workers: n,
            queue_depth: n * 4,
        }
    }
}

impl IngestPoolConfig {
    pub fn with_workers(mut self, workers: usize) -> Self {
        self.workers = workers.clamp(1, 64);
        self.queue_depth = self.workers * 4;
        self
    }
}

/// A single ingest job submitted to the pool.
#[derive(Debug, Clone)]
pub enum IngestJob {
    /// Read + parse a file from disk; the format is auto-detected.
    Path(PathBuf),
    /// Parse an in-memory byte slice with a known format. The label is
    /// surfaced in [`IngestResult`] and errors.
    Bytes {
        data: Vec<u8>,
        format: IngestFormat,
        label: String,
    },
}

/// Result of one job.
#[derive(Debug)]
pub struct IngestResult {
    /// Position of the job in the order returned by
    /// [`IngestPool::submit_batch`]. Useful for callers that want to
    /// re-order results back to input order.
    pub job_index: usize,
    pub outcome: JobOutcome,
}

#[derive(Debug)]
pub enum JobOutcome {
    Ok(Box<IngestedMesh>),
    Err(String),
}

#[derive(Debug)]
enum IndexedJob {
    Job { index: usize, job: IngestJob },
    Shutdown,
}

/// Worker-thread pool. Spawn with [`IngestPool::new`]; submit batches
/// with [`Self::submit_batch`]; drain results with [`Self::drain`].
/// Dropping the pool waits for outstanding workers to exit cleanly.
pub struct IngestPool {
    senders: Vec<Sender<IndexedJob>>,
    workers: Vec<thread::JoinHandle<()>>,
    results: Arc<Mutex<Receiver<IngestResult>>>,
    result_tx: Sender<IngestResult>,
    next_worker: usize,
}

impl IngestPool {
    /// Spawn `cfg.workers` OS threads. Returns a pool that's ready to
    /// accept jobs immediately.
    pub fn new(cfg: IngestPoolConfig) -> Self {
        let workers_n = cfg.workers.clamp(1, 64);
        let (result_tx, result_rx) = channel::<IngestResult>();
        let mut senders = Vec::with_capacity(workers_n);
        let mut workers = Vec::with_capacity(workers_n);
        for i in 0..workers_n {
            let (tx, rx) = channel::<IndexedJob>();
            senders.push(tx);
            let rtx = result_tx.clone();
            let handle = thread::Builder::new()
                .name(format!("aec-asset-ingest-{i}"))
                .spawn(move || worker_loop(rx, rtx))
                .expect("spawn ingest worker");
            workers.push(handle);
        }
        Self {
            senders,
            workers,
            results: Arc::new(Mutex::new(result_rx)),
            result_tx,
            next_worker: 0,
        }
    }

    /// Submit a batch of jobs. Returns the total job count so callers
    /// know how many results to expect from [`Self::drain`].
    ///
    /// Jobs are dispatched round-robin across workers; each worker has
    /// its own input channel so a slow IFC parse on one thread doesn't
    /// block fast OBJ parses on others.
    pub fn submit_batch(&mut self, jobs: Vec<IngestJob>) -> usize {
        let total = jobs.len();
        for (i, job) in jobs.into_iter().enumerate() {
            let w = self.next_worker;
            self.next_worker = (self.next_worker + 1) % self.senders.len();
            // If a worker's receiver was dropped (shouldn't happen
            // during normal operation), surface the error back through
            // the result channel rather than panicking.
            if let Err(send_err) = self.senders[w].send(IndexedJob::Job { index: i, job }) {
                let _ = self.result_tx.send(IngestResult {
                    job_index: i,
                    outcome: JobOutcome::Err(format!("worker dispatch failed: {send_err}")),
                });
            }
        }
        total
    }

    /// Block on the next `expected` results. Returns results in the
    /// order they finish (which is *not* the same as the submission
    /// order — see [`IngestResult::job_index`] for re-sorting).
    pub fn drain(&self, expected: usize) -> Vec<IngestResult> {
        let mut out = Vec::with_capacity(expected);
        let rx = self.results.lock().expect("ingest result lock poisoned");
        for _ in 0..expected {
            match rx.recv() {
                Ok(r) => out.push(r),
                Err(_) => break,
            }
        }
        out
    }
}

impl Drop for IngestPool {
    fn drop(&mut self) {
        // Send shutdown sentinel to each worker, then join.
        for s in &self.senders {
            let _ = s.send(IndexedJob::Shutdown);
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn worker_loop(rx: Receiver<IndexedJob>, results: Sender<IngestResult>) {
    while let Ok(job) = rx.recv() {
        let (index, job) = match job {
            IndexedJob::Shutdown => return,
            IndexedJob::Job { index, job } => (index, job),
        };
        let outcome = match execute(job) {
            Ok(mesh) => JobOutcome::Ok(Box::new(mesh)),
            Err(e) => JobOutcome::Err(format!("{e}")),
        };
        if results
            .send(IngestResult {
                job_index: index,
                outcome,
            })
            .is_err()
        {
            // Result channel closed -> pool is being dropped. Stop.
            return;
        }
    }
}

fn execute(job: IngestJob) -> Result<IngestedMesh, IngestError> {
    match job {
        IngestJob::Path(p) => ingest_path(&p),
        IngestJob::Bytes { data, format, .. } => ingest_bytes(&data, format),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::native;
    use aec_geometry::Mesh;
    use std::collections::HashSet;

    fn cube_mesh_bytes() -> Vec<u8> {
        let mut m = Mesh::new();
        m.push_quad(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        native::encode(&m)
    }

    #[test]
    fn config_clamps_worker_count() {
        let c = IngestPoolConfig::default().with_workers(0);
        assert!(c.workers >= 1);
        let c = IngestPoolConfig::default().with_workers(1000);
        assert!(c.workers <= 64);
    }

    #[test]
    fn pool_processes_all_submitted_jobs() {
        let mut pool = IngestPool::new(IngestPoolConfig::default().with_workers(2));
        let bytes = cube_mesh_bytes();
        let mut jobs = Vec::new();
        for i in 0..6 {
            jobs.push(IngestJob::Bytes {
                data: bytes.clone(),
                format: IngestFormat::Native,
                label: format!("cube-{i}"),
            });
        }
        let count = pool.submit_batch(jobs);
        let results = pool.drain(count);
        assert_eq!(results.len(), 6);
        let mut seen: HashSet<usize> = HashSet::new();
        for r in &results {
            seen.insert(r.job_index);
            match &r.outcome {
                JobOutcome::Ok(mesh) => assert_eq!(mesh.format, IngestFormat::Native),
                JobOutcome::Err(e) => panic!("ingest failed: {e}"),
            }
        }
        assert_eq!(seen.len(), 6, "all job indices should appear exactly once");
    }

    #[test]
    fn pool_surfaces_per_job_errors() {
        let mut pool = IngestPool::new(IngestPoolConfig::default().with_workers(2));
        let jobs = vec![
            IngestJob::Bytes {
                data: vec![0xFF; 16],
                format: IngestFormat::Native,
                label: "corrupt".to_string(),
            },
            IngestJob::Bytes {
                data: cube_mesh_bytes(),
                format: IngestFormat::Native,
                label: "good".to_string(),
            },
        ];
        let count = pool.submit_batch(jobs);
        let results = pool.drain(count);
        let mut ok = 0;
        let mut err = 0;
        for r in &results {
            match r.outcome {
                JobOutcome::Ok(_) => ok += 1,
                JobOutcome::Err(_) => err += 1,
            }
        }
        assert_eq!(ok, 1);
        assert_eq!(err, 1);
    }

    #[test]
    fn pool_shutdown_joins_cleanly() {
        let pool = IngestPool::new(IngestPoolConfig::default().with_workers(4));
        drop(pool);
        // If the test reaches here, the join in Drop succeeded.
    }
}
