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
        }
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            RenderJobStatus::Completed | RenderJobStatus::Failed | RenderJobStatus::Cancelled
        )
    }
}
