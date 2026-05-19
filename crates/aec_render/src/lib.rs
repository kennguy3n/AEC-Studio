//! Render core. Drives the Blender worker, manages the render queue, and
//! implements the EEVEE preview / Cycles final pipelines on the Rust side.
//!
//! The Blender process itself lives in `workers/blender/`; this crate only
//! speaks to it over a JSON-line stdio protocol.

pub mod cycles;
pub mod eevee;
pub mod job;
pub mod preset;
pub mod queue;
pub mod scene;
pub mod worker;

pub use cycles::CyclesPipeline;
pub use eevee::EeveePipeline;
pub use job::{RenderJob, RenderJobStatus};
pub use preset::{RenderPreset, RenderPresetConfig, RenderQuality};
pub use queue::{QueueError, RenderQueue};
pub use scene::{RenderCamera, RenderLight, RenderScene, SerializedMesh};
pub use worker::{BlenderRequest, BlenderResponse, BlenderWorker, WorkerError, WorkerState};
