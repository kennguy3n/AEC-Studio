//! Render queue. In-memory FIFO with priority + status transitions +
//! persistence hooks. The queue is intentionally not a thread pool — the
//! governor decides how many concurrent jobs may run, and calls
//! [`RenderQueue::admit`] to pull the next eligible job.

use std::collections::VecDeque;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::job::{RenderJob, RenderJobStatus};

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("unknown job `{0}`")]
    UnknownJob(String),
    #[error("job `{0}` is in terminal state")]
    Terminal(String),
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
    use crate::preset::RenderPreset;
    use crate::scene::RenderScene;

    fn make_job(priority: i32) -> RenderJob {
        RenderJob::new(RenderPreset::standard(), RenderScene::new()).with_priority(priority)
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
}
