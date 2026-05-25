//! Render core. Native CPU/GPU path tracer + PBR preview pipeline.
//!
//! Phase 9 removed the external Blender worker dependency entirely;
//! every render code path — preview, final, walkthrough, panorama —
//! runs natively in this crate. Path tracing lives in [`path_trace`]
//! (CPU) and [`gpu_trace`] (wgpu compute), with shared scene compilation
//! through [`bvh`], [`intersect`], [`material`], [`light_sampling`], and
//! [`denoise`]. The preview rasteriser is the [`preview`] module; final
//! offline renders go through [`final_render`]; multi-frame walkthroughs
//! and equirectangular panoramas through [`walkthrough`] and
//! [`panorama`].

pub mod bvh;
pub mod cameras;
pub mod denoise;
pub mod doctor;
pub mod final_render;
pub mod gpu_trace;
pub mod history;
pub mod intersect;
pub mod job;
pub mod job_store;
pub mod light_sampling;
pub mod lighting;
pub mod material;
pub mod panorama;
pub mod path_trace;
pub mod preset;
pub mod preview;
pub mod queue;
pub mod sampling;
pub mod scene;
pub mod scheduler;
pub mod walkthrough;

pub use bvh::{Aabb, BuilderTriangle, Bvh, BvhNode};
pub use cameras::{
    render_thumbnail_rgba8, CameraJournal, CameraJournalEntry, CameraPresetKind, CameraSnapshot,
    CameraStore, CameraValidationError,
};
pub use denoise::{bilateral_denoise, nlm_denoise, BilateralParams, Denoiser, ImageRgb, NlmParams};
pub use doctor::{
    check_materials, CheckMaterialsOptions, MaterialCheckResult, MaterialFinding,
    DEFAULT_MAX_TEXTURE_EDGE_PX,
};
pub use final_render::{FinalRenderError, FinalRenderOutput, FinalRenderPipeline};
pub use gpu_trace::{
    render_or_fallback as gpu_render_or_fallback, validate_shader as gpu_validate_shader,
    GpuPathTracer, GpuSceneBuffers, GpuTraceError, SHADER_SOURCE as GPU_SHADER_SOURCE,
};
pub use history::{
    compare as compare_history, CompareResult, FieldDiff, HistoryError, RenderHistory,
    RenderHistoryEntry,
};
pub use intersect::{any_hit, closest_hit, geom_normal, Intersection, Ray, ShadingTriangle};
pub use job::{RenderJob, RenderJobStatus};
pub use job_store::{JobStatusCounts, RenderJobStore, RenderJobStoreError};
pub use light_sampling::{
    environment_radiance, is_delta, power_heuristic, sample_light, LightSample, NativeLight,
};
pub use lighting::{
    kelvin_to_rgb, IesParseError, IesPhotometricType, IesProfile, LightingPreset,
    LightingPresetKind, LightingPresetStore, LightingValidationError, SkyParams,
};
pub use material::{
    eval_bsdf, fresnel_schlick, ggx_d, ggx_g_smith, pdf_bsdf, sample_bsdf, tangent_basis,
    BsdfSample, PathTraceMaterial,
};
pub use panorama::{PanoramaError, PanoramaOutput, PanoramaPipeline};
pub use path_trace::{
    render as render_path_trace, render_tile_pass, AccumulationBuffer, CameraProjection,
    CancelToken, PathTraceConfig, PathTraceScene, ProgressFn, Tile, TilePassResult,
    TriangleShading,
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
pub use walkthrough::{
    WalkthroughError, WalkthroughOutput, WalkthroughPipeline, WalkthroughProgress,
    WalkthroughStitchOptions,
};
