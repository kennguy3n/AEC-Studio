//! Real-time viewport for AEC Studio.
//!
//! The viewport is split into a *math-and-state* layer (this crate's pure
//! Rust modules) and a *gpu* layer (`renderer.rs`, `shaders/`) that owns
//! the actual wgpu device. Domain code in `aec_geometry` /
//! `aec_command` only ever talks to the state layer; the GPU code reads
//! state but never mutates application data.

pub mod bim_batch;
pub mod cad_canvas;
pub mod camera;
pub mod csm;
pub mod culling;
pub mod draft_2d;
pub mod gizmo;
pub mod grid;
pub mod instancing;
pub mod material_preview;
pub mod outline;
pub mod pbr_preview;
pub mod picking;
pub mod reference_image;
pub mod render_pipeline;
pub mod renderer;
pub mod scene;
pub mod selection;
pub mod sky;
pub mod snap_overlay;
pub mod surface;
pub mod viewport_pipeline;

pub use bim_batch::{BatchKey, BimBatch, BimBatchCache, BimEntity, BimKind, MeshHash};
pub use cad_canvas::{
    compute_crosshair, compute_grid, CadCanvasState, CadGrid, CrosshairLines, GridLines,
    OrthoCamera2D, RubberBand, WorldRect,
};
pub use camera::{Camera, CameraMode, OrbitController, Ray};
pub use csm::{CascadeSlice, CsmParams, CsmSplit};
pub use culling::{cull_visible, project_for_hiz, Aabb, Frustum, HiZQuery, Plane};
pub use gizmo::{GizmoAxis, GizmoMode, TransformGizmo};
pub use grid::{Grid, GridStyle};
pub use instancing::{Instance, InstanceBatch};
pub use material_preview::{MaterialPreview, MaterialPreviewSlot};
pub use outline::{jump_flood_sdf, outline_thickness_mask, OutlinePixel, OutlineSdf, OutlineStyle};
pub use picking::{PickRegistry, PickedHit, PickingId, PickingTarget};
pub use reference_image::{
    decode as decode_reference_image, enumerate_pages as enumerate_reference_image_pages,
    ReferenceImage, ReferenceImageBitmap, ReferenceImageError, ReferenceImageKind,
    ReferenceImageOverlay, ReferenceImagePage, MAX_IMAGE_DIMENSION_PX,
};
pub use render_pipeline::{
    CameraUniform, PipelineConfig, PipelineError, RenderPipeline, DEFAULT_MSAA,
};
pub use renderer::{GpuDescriptor, RendererBackend, RendererError, ViewportRenderer};
pub use scene::{SceneGraph, SceneMesh, SceneNode, SceneNodeKind};
pub use selection::{Selection, SelectionMode};
pub use snap_overlay::{SnapHit, SnapKind, SnapOverlay};
pub use surface::{aligned_row_bytes, FrameKey, SurfaceError, SurfaceManager};
pub use viewport_pipeline::{
    validate_shaders as validate_viewport_shaders, ViewportDescriptor, ViewportError,
    ViewportFrame, ViewportPipeline, ViewportStats, HIZ_SHADER_SOURCE, OUTLINE_SHADER_SOURCE,
    PBR_CSM_SHADER_SOURCE, PICKING_SHADER_SOURCE, SHADOW_CSM_SHADER_SOURCE,
};
