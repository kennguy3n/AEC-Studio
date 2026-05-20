//! Render core. Drives the Blender worker, manages the render queue, and
//! implements the EEVEE preview / Cycles final pipelines on the Rust side.
//!
//! The Blender process itself lives in `workers/blender/`; this crate only
//! speaks to it over a JSON-line stdio protocol.

pub mod blender_discovery;
pub mod cameras;
pub mod cycles;
pub mod doctor;
pub mod eevee;
pub mod history;
pub mod job;
pub mod lighting;
pub mod preset;
pub mod queue;
pub mod scene;
pub mod worker;

pub use blender_discovery::{
    discover_blender, discover_blender_with, BlenderDiscovery, DiscoverySource,
};
pub use cameras::{
    render_thumbnail_rgba8, CameraJournal, CameraJournalEntry, CameraPresetKind, CameraSnapshot,
    CameraStore, CameraValidationError,
};
pub use cycles::CyclesPipeline;
pub use doctor::{
    check_materials, CheckMaterialsOptions, MaterialCheckResult, MaterialFinding,
    DEFAULT_MAX_TEXTURE_EDGE_PX,
};
pub use eevee::EeveePipeline;
pub use history::{
    compare as compare_history, CompareResult, FieldDiff, HistoryError, RenderHistory,
    RenderHistoryEntry,
};
pub use job::{RenderJob, RenderJobStatus};
pub use lighting::{
    kelvin_to_rgb, IesParseError, IesPhotometricType, IesProfile, LightingPayload, LightingPreset,
    LightingPresetKind, LightingPresetStore, LightingValidationError, SkyParams, WorkerLight,
    WorkerWorld,
};
pub use preset::{
    migrate_legacy_preset_id, recommend_preset, PresetError, RenderPreset, RenderPresetConfig,
    RenderPresetStore, RenderQuality,
};
pub use queue::{QueueError, RenderQueue};
pub use scene::{RenderCamera, RenderLight, RenderScene, SerializedMesh};
pub use worker::{
    BlenderRequest, BlenderResponse, BlenderWorker, WalkthroughOutput, WorkerError, WorkerState,
};
