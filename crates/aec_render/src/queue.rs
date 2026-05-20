//! Render queue. In-memory FIFO with priority + status transitions +
//! persistence hooks. The queue is intentionally not a thread pool — the
//! governor decides how many concurrent jobs may run, and calls
//! [`RenderQueue::admit`] to pull the next eligible job.

use std::collections::VecDeque;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cameras::CameraSnapshot;
use crate::job::{RenderJob, RenderJobStatus};
use crate::preset::RenderPreset;
use crate::scene::RenderScene;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("unknown job `{0}`")]
    UnknownJob(String),
    #[error("job `{0}` is in terminal state")]
    Terminal(String),
}

/// Returned by [`RenderQueue::submit_batch`] / [`RenderQueue::submit_matrix`].
/// Carries the shared batch id plus the per-camera job ids so the caller
/// can drive per-job IPC without re-querying the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSubmission {
    pub batch_id: String,
    pub job_ids: Vec<String>,
}

/// Snapshot of one batch's progress derived from the queue's job states.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchProgress {
    pub batch_id: String,
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Average progress across all jobs in the batch (0.0..1.0).
    /// Completed jobs contribute 1.0; failed/cancelled jobs contribute 0.0.
    pub average_progress: f32,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderQueue {
    queued: VecDeque<RenderJob>,
    running: Vec<RenderJob>,
    completed: Vec<RenderJob>,
}

impl RenderQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit a job. Returns the job id.
    pub fn submit(&mut self, mut job: RenderJob) -> String {
        job.status = RenderJobStatus::Queued;
        let id = job.id.clone();
        // Insert maintaining descending priority order.
        let pos = self
            .queued
            .iter()
            .position(|j| j.priority < job.priority)
            .unwrap_or(self.queued.len());
        self.queued.insert(pos, job);
        id
    }

    /// Submit one job per camera against a single preset. Shares the
    /// supplied scene by reference — every job receives a clone so
    /// later edits to the original scene don't bleed through.
    /// Each job is tagged with the same `batch_id` so the UI can
    /// render group progress.
    pub fn submit_batch(
        &mut self,
        cameras: &[CameraSnapshot],
        preset: RenderPreset,
        scene: &RenderScene,
    ) -> BatchSubmission {
        let batch_id = format!("batch_{}", uuid::Uuid::new_v4().simple());
        let mut ids = Vec::with_capacity(cameras.len());
        for cam in cameras {
            let job = RenderJob::new(preset.clone(), scene.clone())
                .with_camera_id(cam.id.as_str().to_string())
                .with_batch_id(batch_id.clone());
            ids.push(self.submit(job));
        }
        BatchSubmission { batch_id, job_ids: ids }
    }

    /// Submit a camera × preset matrix — one job per combination.
    /// Useful for "render every saved camera at quick AND high quality
    /// so we can pick the best later". Shares `scene` by clone.
    pub fn submit_matrix(
        &mut self,
        cameras: &[CameraSnapshot],
        presets: &[RenderPreset],
        scene: &RenderScene,
    ) -> BatchSubmission {
        let batch_id = format!("batch_{}", uuid::Uuid::new_v4().simple());
        let mut ids = Vec::with_capacity(cameras.len() * presets.len());
        for cam in cameras {
            for preset in presets {
                let job = RenderJob::new(preset.clone(), scene.clone())
                    .with_camera_id(cam.id.as_str().to_string())
                    .with_batch_id(batch_id.clone());
                ids.push(self.submit(job));
            }
        }
        BatchSubmission { batch_id, job_ids: ids }
    }

    /// All jobs (across all states) tagged with the given batch id.
    pub fn jobs_in_batch(&self, batch_id: &str) -> Vec<&RenderJob> {
        self.list_jobs()
            .into_iter()
            .filter(|j| j.batch_id.as_deref() == Some(batch_id))
            .collect()
    }

    /// Aggregate batch progress: average per-job progress weighted by
    /// terminal/non-terminal state. Returns `None` for an unknown batch.
    pub fn batch_progress(&self, batch_id: &str) -> Option<BatchProgress> {
        let jobs = self.jobs_in_batch(batch_id);
        if jobs.is_empty() {
            return None;
        }
        let total = jobs.len();
        let mut completed = 0usize;
        let mut failed = 0usize;
        let mut cancelled = 0usize;
        let mut running = 0usize;
        let mut queued = 0usize;
        let mut progress_sum = 0.0f32;
        for j in &jobs {
            match j.status {
                RenderJobStatus::Completed => {
                    completed += 1;
                    progress_sum += 1.0;
                }
                RenderJobStatus::Failed => {
                    failed += 1;
                }
                RenderJobStatus::Cancelled => {
                    cancelled += 1;
                }
                RenderJobStatus::Running => {
                    running += 1;
                    progress_sum += j.progress;
                }
                RenderJobStatus::Queued => {
                    queued += 1;
                }
            }
        }
        Some(BatchProgress {
            batch_id: batch_id.to_string(),
            total,
            completed,
            failed,
            cancelled,
            running,
            queued,
            average_progress: progress_sum / total as f32,
        })
    }

    pub fn queued_count(&self) -> usize {
        self.queued.len()
    }

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn completed_count(&self) -> usize {
        self.completed.len()
    }

    /// Promote the highest-priority queued job to Running. Returns a clone
    /// of the admitted job — the caller (a render driver) will mutate the
    /// queue via [`update_progress`] and [`complete`] as the job advances.
    pub fn admit(&mut self) -> Option<RenderJob> {
        let mut job = self.queued.pop_front()?;
        job.status = RenderJobStatus::Running;
        job.started_at = Some(Utc::now());
        self.running.push(job.clone());
        Some(job)
    }

    pub fn update_progress(&mut self, id: &str, progress: f32) -> Result<(), QueueError> {
        let j = self
            .running
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or_else(|| QueueError::UnknownJob(id.into()))?;
        j.progress = progress.clamp(0.0, 1.0);
        Ok(())
    }

    pub fn complete(
        &mut self,
        id: &str,
        output_path: impl Into<std::path::PathBuf>,
    ) -> Result<(), QueueError> {
        let idx = self
            .running
            .iter()
            .position(|j| j.id == id)
            .ok_or_else(|| QueueError::UnknownJob(id.into()))?;
        let mut job = self.running.remove(idx);
        job.status = RenderJobStatus::Completed;
        job.progress = 1.0;
        job.completed_at = Some(Utc::now());
        job.output_path = Some(output_path.into());
        self.completed.push(job);
        Ok(())
    }

    pub fn fail(&mut self, id: &str, message: impl Into<String>) -> Result<(), QueueError> {
        if let Some(idx) = self.running.iter().position(|j| j.id == id) {
            let mut job = self.running.remove(idx);
            job.status = RenderJobStatus::Failed;
            job.completed_at = Some(Utc::now());
            job.error = Some(message.into());
            self.completed.push(job);
            return Ok(());
        }
        if let Some(idx) = self.queued.iter().position(|j| j.id == id) {
            let mut job = self.queued.remove(idx).unwrap();
            job.status = RenderJobStatus::Failed;
            job.completed_at = Some(Utc::now());
            job.error = Some(message.into());
            self.completed.push(job);
            return Ok(());
        }
        Err(QueueError::UnknownJob(id.into()))
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), QueueError> {
        if let Some(idx) = self.queued.iter().position(|j| j.id == id) {
            let mut job = self.queued.remove(idx).unwrap();
            job.status = RenderJobStatus::Cancelled;
            job.completed_at = Some(Utc::now());
            self.completed.push(job);
            return Ok(());
        }
        if let Some(idx) = self.running.iter().position(|j| j.id == id) {
            let mut job = self.running.remove(idx);
            job.status = RenderJobStatus::Cancelled;
            job.completed_at = Some(Utc::now());
            self.completed.push(job);
            return Ok(());
        }
        Err(QueueError::UnknownJob(id.into()))
    }

    /// Resume a failed job by re-queueing it with its previous state cleared.
    pub fn resume(&mut self, id: &str) -> Result<(), QueueError> {
        let idx = self
            .completed
            .iter()
            .position(|j| j.id == id && j.status == RenderJobStatus::Failed)
            .ok_or_else(|| QueueError::Terminal(id.into()))?;
        let mut job = self.completed.remove(idx);
        job.status = RenderJobStatus::Queued;
        job.error = None;
        job.progress = 0.0;
        job.started_at = None;
        job.completed_at = None;
        self.queued.push_back(job);
        Ok(())
    }

    pub fn list_jobs(&self) -> Vec<&RenderJob> {
        self.queued
            .iter()
            .chain(self.running.iter())
            .chain(self.completed.iter())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&RenderJob> {
        self.list_jobs().into_iter().find(|j| j.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::CameraSnapshot;
    use aec_core::EntityId;

    fn make_job(priority: i32) -> RenderJob {
        RenderJob::new(RenderPreset::standard(), RenderScene::new()).with_priority(priority)
    }

    fn make_camera(name: &str) -> CameraSnapshot {
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

    #[test]
    fn queue_preserves_priority_order() {
        let mut q = RenderQueue::new();
        let low = q.submit(make_job(0));
        let high = q.submit(make_job(10));
        let mid = q.submit(make_job(5));
        let first = q.admit().unwrap();
        assert_eq!(first.id, high);
        let second = q.admit().unwrap();
        assert_eq!(second.id, mid);
        let third = q.admit().unwrap();
        assert_eq!(third.id, low);
        assert_eq!(q.queued_count(), 0);
        assert_eq!(q.running_count(), 3);
    }

    #[test]
    fn complete_moves_to_completed() {
        let mut q = RenderQueue::new();
        let id = q.submit(make_job(0));
        let _ = q.admit().unwrap();
        q.update_progress(&id, 0.5).unwrap();
        q.complete(&id, "out.png").unwrap();
        let job = q.get(&id).unwrap();
        assert_eq!(job.status, RenderJobStatus::Completed);
        assert!(job.output_path.is_some());
        assert!((job.progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cancel_from_queued_or_running() {
        let mut q = RenderQueue::new();
        let a = q.submit(make_job(0));
        let b = q.submit(make_job(0));
        let _ = q.admit().unwrap();
        q.cancel(&a).unwrap();
        q.cancel(&b).unwrap();
        assert_eq!(q.get(&a).unwrap().status, RenderJobStatus::Cancelled);
        assert_eq!(q.get(&b).unwrap().status, RenderJobStatus::Cancelled);
    }

    #[test]
    fn resume_failed_job() {
        let mut q = RenderQueue::new();
        let id = q.submit(make_job(0));
        let _ = q.admit().unwrap();
        q.fail(&id, "oom").unwrap();
        assert_eq!(q.get(&id).unwrap().status, RenderJobStatus::Failed);
        q.resume(&id).unwrap();
        assert_eq!(q.get(&id).unwrap().status, RenderJobStatus::Queued);
    }

    #[test]
    fn batch_submission_holds_state() {
        let mut q = RenderQueue::new();
        for _ in 0..10 {
            q.submit(make_job(0));
        }
        assert_eq!(q.queued_count(), 10);
    }

    #[test]
    fn submit_batch_creates_one_job_per_camera() {
        let mut q = RenderQueue::new();
        let cams = vec![
            make_camera("Living"),
            make_camera("Kitchen"),
            make_camera("Bath"),
        ];
        let scene = RenderScene::new();
        let sub = q.submit_batch(&cams, RenderPreset::standard(), &scene);

        assert_eq!(sub.job_ids.len(), 3);
        assert_eq!(q.queued_count(), 3);
        let batch_jobs = q.jobs_in_batch(&sub.batch_id);
        assert_eq!(batch_jobs.len(), 3);
        // every job carries the same batch_id and a distinct camera_id.
        let cam_ids: std::collections::HashSet<_> =
            batch_jobs.iter().filter_map(|j| j.camera_id.clone()).collect();
        assert_eq!(cam_ids.len(), 3);
        for j in &batch_jobs {
            assert_eq!(j.batch_id.as_deref(), Some(sub.batch_id.as_str()));
        }
    }

    #[test]
    fn submit_matrix_creates_cameras_times_presets_jobs() {
        let mut q = RenderQueue::new();
        let cams = vec![make_camera("A"), make_camera("B")];
        let presets = vec![RenderPreset::quick(), RenderPreset::high()];
        let scene = RenderScene::new();
        let sub = q.submit_matrix(&cams, &presets, &scene);

        assert_eq!(sub.job_ids.len(), 4);
        assert_eq!(q.queued_count(), 4);

        // Every (camera, preset) pair must be represented exactly once.
        let batch_jobs = q.jobs_in_batch(&sub.batch_id);
        let pairs: std::collections::HashSet<_> = batch_jobs
            .iter()
            .map(|j| {
                (
                    j.camera_id.clone().unwrap_or_default(),
                    j.preset.id.clone(),
                )
            })
            .collect();
        assert_eq!(pairs.len(), 4);
    }

    #[test]
    fn batch_progress_aggregates_states() {
        let mut q = RenderQueue::new();
        let cams = vec![make_camera("A"), make_camera("B"), make_camera("C")];
        let scene = RenderScene::new();
        let sub = q.submit_batch(&cams, RenderPreset::standard(), &scene);

        // Admit two; complete one, leave the other half-running; the third remains queued.
        let first = q.admit().unwrap();
        let _second = q.admit().unwrap();
        q.complete(&first.id, "out_a.png").unwrap();
        q.update_progress(&_second.id, 0.5).unwrap();

        let p = q.batch_progress(&sub.batch_id).unwrap();
        assert_eq!(p.total, 3);
        assert_eq!(p.completed, 1);
        assert_eq!(p.running, 1);
        assert_eq!(p.queued, 1);
        // Average = (1.0 + 0.5 + 0.0) / 3 = 0.5.
        assert!((p.average_progress - 0.5).abs() < 1e-4);
    }

    #[test]
    fn batch_progress_unknown_returns_none() {
        let q = RenderQueue::new();
        assert!(q.batch_progress("batch_nope").is_none());
    }
}
