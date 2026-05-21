//! Render core. Manages the render queue, the native CPU/GPU path tracer,
//! and the native PBR preview pipeline.
//!
//! Phase 9 removed the external Blender worker dependency; all path
//! tracing, BSDF evaluation, BVH traversal, and IES sampling now lives
//! natively in this crate (see [`bvh`], [`intersect`], [`material`],
//! [`light_sampling`], [`path_trace`]). The legacy [`worker`],
//! [`blender_discovery`], [`cycles`], and [`eevee`] modules remain
//! temporarily during the cutover and will be removed in PR4 once the
//! native pipeline owns every render code path end-to-end.

pub mod blender_discovery;
pub mod bvh;
pub mod cameras;
pub mod cycles;
pub mod denoise;
pub mod doctor;
pub mod eevee;
pub mod gpu_trace;
pub mod history;
pub mod intersect;
pub mod job;
pub mod light_sampling;
pub mod lighting;
pub mod material;
pub mod path_trace;
pub mod preset;
pub mod queue;
pub mod scene;
pub mod scheduler;
pub mod worker;

pub use blender_discovery::{
    discover_blender, discover_blender_with, BlenderDiscovery, DiscoverySource,
};
pub use bvh::{Aabb, BuilderTriangle, Bvh, BvhNode};
pub use cameras::{
    render_thumbnail_rgba8, CameraJournal, CameraJournalEntry, CameraPresetKind, CameraSnapshot,
    CameraStore, CameraValidationError,
};
pub use cycles::CyclesPipeline;
pub use denoise::{bilateral_denoise, nlm_denoise, BilateralParams, Denoiser, ImageRgb, NlmParams};
pub use doctor::{
    check_materials, CheckMaterialsOptions, MaterialCheckResult, MaterialFinding,
    DEFAULT_MAX_TEXTURE_EDGE_PX,
};
pub use eevee::EeveePipeline;
pub use gpu_trace::{
    render_or_fallback as gpu_render_or_fallback, validate_shader as gpu_validate_shader,
    GpuPathTracer, GpuSceneBuffers, GpuTraceError, SHADER_SOURCE as GPU_SHADER_SOURCE,
};
pub mod preview;
pub use history::{
    compare as compare_history, CompareResult, FieldDiff, HistoryError, RenderHistory,
    RenderHistoryEntry,
};
pub use intersect::{any_hit, closest_hit, geom_normal, Intersection, Ray, ShadingTriangle};
pub use job::{RenderJob, RenderJobStatus};
pub use light_sampling::{
    environment_radiance, is_delta, power_heuristic, sample_light, LightSample, NativeLight,
};
pub use lighting::{
    kelvin_to_rgb, IesParseError, IesPhotometricType, IesProfile, LightingPayload, LightingPreset,
    LightingPresetKind, LightingPresetStore, LightingValidationError, SkyParams, WorkerLight,
    WorkerWorld,
};
pub use material::{
    eval_bsdf, fresnel_schlick, ggx_d, ggx_g_smith, pdf_bsdf, sample_bsdf, tangent_basis,
    BsdfSample, PathTraceMaterial,
};
pub use path_trace::{
    render as render_path_trace, render_tile_pass, AccumulationBuffer, CancelToken,
    PathTraceConfig, PathTraceScene, ProgressFn, Tile, TilePassResult, TriangleShading,
};
pub use preset::{
    migrate_legacy_preset_id, recommend_preset, PresetError, RenderPreset, RenderPresetConfig,
    RenderPresetStore, RenderQuality,
};
pub use preview::{
    pick_sky_state, scene_world_sphere, tier_resolution_scale, tier_tile_size, PreviewBuildError,
    PreviewFrameOutput, PreviewMaterial, PreviewPipeline, PreviewTile, SharedPreviewPipeline,
};
pub use queue::{QueueError, RenderQueue};
pub use scene::{RenderCamera, RenderLight, RenderScene, SerializedMesh};
pub use scheduler::{
    config_from_preset as scheduler_config_from_preset, make_progress_observer, schedule,
    SchedulerConfig, SchedulerOutcome, SchedulerProgress, SchedulerProgressFn, SchedulerStats,
};
pub use worker::{
    BlenderRequest, BlenderResponse, BlenderWorker, WalkthroughOutput, WorkerError, WorkerState,
};
