//! Render job: status machine + persisted state used by the queue.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::preset::RenderPreset;
use crate::scene::RenderScene;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderJob {
    pub id: String,
    pub preset: RenderPreset,
    pub scene: RenderScene,
    pub status: RenderJobStatus,
    /// 0.0..1.0
    pub progress: f32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub output_path: Option<PathBuf>,
    pub error: Option<String>,
    /// Priority (higher = sooner). Defaults to 0.
    pub priority: i32,
    /// Camera this job is rendering, when known. The batch / matrix
    /// submission paths set this so the UI can group jobs by camera
    /// in the queue view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_id: Option<String>,
    /// Batch group id. Set when the job belongs to a batch submission
    /// — every job in the same batch shares the id so the UI can
    /// show overall batch progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// Walkthrough resume marker: the sorted, deduplicated list of
    /// frame indices that have been written to disk for this job.
    /// `RenderQueue::resume` uses the maximum value as the next
    /// `frame_start` so a failed walkthrough resumes after the last
    /// successful frame. Non-walkthrough jobs leave this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_frames: Vec<u32>,
}

impl RenderJob {
    pub fn new(preset: RenderPreset, scene: RenderScene) -> Self {
        Self {
            id: format!("job_{}", uuid::Uuid::new_v4().simple()),
            preset,
            scene,
            status: RenderJobStatus::Queued,
            progress: 0.0,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            output_path: None,
            error: None,
            priority: 0,
            camera_id: None,
            batch_id: None,
            completed_frames: Vec::new(),
        }
    }

    /// Mark a walkthrough frame as completed. Maintains the
    /// `completed_frames` field sorted + unique so callers can use
    /// `last()` as the resume point.
    pub fn mark_frame_completed(&mut self, frame: u32) {
        if !self.completed_frames.contains(&frame) {
            self.completed_frames.push(frame);
            self.completed_frames.sort_unstable();
        }
    }

    /// The next frame to render if this job is resumed. Returns
    /// `Some(n)` when at least one frame has been completed.
    pub fn next_walkthrough_frame(&self) -> Option<u32> {
        self.completed_frames.last().map(|f| f + 1)
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_camera_id(mut self, camera_id: impl Into<String>) -> Self {
        self.camera_id = Some(camera_id.into());
        self
    }

    pub fn with_batch_id(mut self, batch_id: impl Into<String>) -> Self {
        self.batch_id = Some(batch_id.into());
        self
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            RenderJobStatus::Completed | RenderJobStatus::Failed | RenderJobStatus::Cancelled
        )
    }
}
