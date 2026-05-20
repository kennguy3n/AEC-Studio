//! Real-time viewport for AEC Studio.
//!
//! The viewport is split into a *math-and-state* layer (this crate's pure
//! Rust modules) and a *gpu* layer (`renderer.rs`, `shaders/`) that owns
//! the actual wgpu device. Domain code in `aec_geometry` /
//! `aec_command` only ever talks to the state layer; the GPU code reads
//! state but never mutates application data.

pub mod cad_canvas;
pub mod camera;
pub mod gizmo;
pub mod grid;
pub mod instancing;
pub mod material_preview;
pub mod reference_image;
pub mod renderer;
pub mod scene;
pub mod selection;
pub mod snap_overlay;

pub use cad_canvas::{
    compute_crosshair, compute_grid, CadCanvasState, CadGrid, CrosshairLines, GridLines,
    OrthoCamera2D, RubberBand, WorldRect,
};
pub use camera::{Camera, CameraMode, OrbitController, Ray};
pub use gizmo::{GizmoAxis, GizmoMode, TransformGizmo};
pub use grid::{Grid, GridStyle};
pub use instancing::{Instance, InstanceBatch};
pub use material_preview::{MaterialPreview, MaterialPreviewSlot};
pub use reference_image::{
    decode as decode_reference_image, ReferenceImage, ReferenceImageBitmap, ReferenceImageError,
    ReferenceImageKind, ReferenceImageOverlay, MAX_IMAGE_DIMENSION_PX,
};
pub use renderer::{RendererBackend, ViewportRenderer};
pub use scene::{SceneGraph, SceneMesh, SceneNode, SceneNodeKind};
pub use selection::{Selection, SelectionMode};
pub use snap_overlay::{SnapHit, SnapKind, SnapOverlay};
