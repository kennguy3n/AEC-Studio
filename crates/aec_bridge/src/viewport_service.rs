//! Bridge-layer state for the real-time 3D viewport.
//!
//! Owns the wgpu [`ViewportRenderer`], the [`RenderPipeline`], and the
//! per-camera [`SurfaceManager`] for the active project. The
//! `BridgeService` exposes a small public API
//! (`viewport_resize` / `viewport_request_frame` / `viewport_input`)
//! that the Electron IPC handlers and Devin's bridge tests call into;
//! the actual GPU work happens behind that surface.
//!
//! The service degrades gracefully when no GPU adapter is available
//! (typical for CI runners): the renderer fields stay `None`, the
//! status report reflects `unavailable`, and request_frame returns a
//! deterministic empty frame so the renderer UI still has something
//! to display.

use std::sync::{Arc, RwLock};

use aec_viewport::camera::Camera;
use aec_viewport::render_pipeline::{CameraUniform, PipelineConfig, RenderPipeline};
use aec_viewport::renderer::{RendererBackend, ViewportRenderer};
use aec_viewport::surface::{FrameKey, SurfaceManager};
use glam::{Mat4, Quat, Vec3};

// Re-export Camera-adjacent helpers so the napi layer + bridge tests
// don't need to depend on aec_viewport directly.
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors surfaced by the viewport service.
#[derive(Debug, Error)]
pub enum ViewportServiceError {
    #[error("invalid viewport size: {0}x{1}")]
    InvalidSize(u32, u32),
    #[error("no GPU adapter available — viewport is in fallback mode")]
    NoAdapter,
    #[error("internal viewport error: {0}")]
    Internal(String),
}

/// Status report surfaced via N-API for the renderer's diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportStatusReport {
    /// `"ready"` when a real device + pipeline exist; `"unavailable"`
    /// otherwise.
    pub state: String,
    /// GPU vendor/model string (matches the governor's hardware
    /// profile output).
    pub gpu_descriptor: Option<aec_viewport::renderer::GpuDescriptor>,
    /// Current viewport extent.
    pub width: u32,
    pub height: u32,
    pub frame_index: u64,
}

/// Mouse / camera input routed from the renderer to the viewport.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ViewportInput {
    Orbit { dx: f32, dy: f32 },
    Pan { dx: f32, dy: f32 },
    Zoom { delta: f32 },
    Reset,
}

/// Result of `viewport_request_frame` — a deterministic summary the
/// renderer can render in its diagnostics panel. The actual pixel
/// bytes flow through the readback buffer (not encoded into the IPC
/// payload — it would blow the IPC frame size).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportFrameReport {
    pub frame_index: u64,
    pub width: u32,
    pub height: u32,
    pub camera: ViewportCameraReport,
    /// `"presented"` when the renderer produced a new frame,
    /// `"coalesced"` when the prior frame was reused (frame
    /// coalescing), `"unavailable"` when no GPU device is present.
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportCameraReport {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub up: [f32; 3],
    pub fov_y_radians: f32,
}

/// Inner state. Wrapped in an `Arc<RwLock<_>>` so the bridge service
/// can clone the handle freely.
struct Inner {
    renderer: Option<ViewportRenderer>,
    pipeline: Option<RenderPipeline>,
    surface: Option<SurfaceManager>,
    camera: Camera,
    width: u32,
    height: u32,
    config: PipelineConfig,
    /// Monotonic hash counter used as the camera component of the
    /// frame key — we don't try to hash the f32 fields directly,
    /// since they include floating-point comparisons; instead each
    /// successful camera update bumps this counter.
    camera_hash: u64,
}

/// Bridge-layer viewport state.
#[derive(Clone)]
pub struct ViewportService {
    inner: Arc<RwLock<Inner>>,
}

impl ViewportService {
    /// Construct the viewport service. Eagerly tries to acquire a GPU
    /// adapter/device — on CI runners with no adapter the service
    /// stays in "unavailable" mode but is still safe to call.
    pub fn new() -> Self {
        let renderer = ViewportRenderer::new_headless(RendererBackend::Fallback).ok();
        let config = PipelineConfig::default();
        let pipeline = renderer
            .as_ref()
            .and_then(|r| RenderPipeline::build(r, config).ok());
        Self {
            inner: Arc::new(RwLock::new(Inner {
                renderer,
                pipeline,
                surface: None,
                camera: default_camera(),
                width: 0,
                height: 0,
                config,
                camera_hash: 0,
            })),
        }
    }

    /// Resize (or initially size) the off-screen surface.
    ///
    /// # Errors
    ///
    /// Returns [`ViewportServiceError::InvalidSize`] when either
    /// dimension is zero. Other failures surface as
    /// [`ViewportServiceError::Internal`].
    pub fn resize(&self, width: u32, height: u32) -> Result<(), ViewportServiceError> {
        if width == 0 || height == 0 {
            return Err(ViewportServiceError::InvalidSize(width, height));
        }
        let mut inner = self.inner.write().expect("viewport service poisoned");
        inner.width = width;
        inner.height = height;
        // Decompose the borrow so the device (immutable) and the
        // surface slot (mutable) come from independent paths into
        // the same struct — Rust's NLL knows these don't overlap.
        let Inner {
            renderer,
            surface,
            config,
            ..
        } = &mut *inner;
        let Some(renderer) = renderer.as_ref() else {
            return Ok(());
        };
        let Some(device) = renderer.device.as_ref() else {
            return Ok(());
        };
        if let Some(s) = surface.as_mut() {
            s.resize(device, width, height)
                .map_err(|e| ViewportServiceError::Internal(e.to_string()))?;
        } else {
            let s = SurfaceManager::new(device, width, height, *config)
                .map_err(|e| ViewportServiceError::Internal(e.to_string()))?;
            *surface = Some(s);
        }
        Ok(())
    }

    /// Apply an input event to the orbit camera.
    pub fn apply_input(&self, input: ViewportInput) {
        let mut inner = self.inner.write().expect("viewport service poisoned");
        let current = inner.camera.clone();
        inner.camera = match input {
            ViewportInput::Orbit { dx, dy } => orbit(current, dx, dy),
            ViewportInput::Pan { dx, dy } => pan(current, dx, dy),
            ViewportInput::Zoom { delta } => zoom(current, delta),
            ViewportInput::Reset => default_camera(),
        };
        inner.camera_hash = inner.camera_hash.wrapping_add(1);
    }

    /// Request a frame. Records the current camera state in the
    /// pipeline's uniform buffer (when a GPU device is present) and
    /// returns a deterministic report. Frame coalescing short-circuits
    /// when nothing has changed since the last call.
    pub fn request_frame(&self) -> Result<ViewportFrameReport, ViewportServiceError> {
        let mut inner = self.inner.write().expect("viewport service poisoned");
        let camera_report = camera_report(&inner.camera);
        // Fallback mode: no device.
        let Some(renderer) = inner.renderer.as_ref() else {
            return Ok(ViewportFrameReport {
                frame_index: 0,
                width: inner.width,
                height: inner.height,
                camera: camera_report,
                state: "unavailable".into(),
            });
        };
        let Some(pipeline) = inner.pipeline.as_ref() else {
            return Ok(ViewportFrameReport {
                frame_index: 0,
                width: inner.width,
                height: inner.height,
                camera: camera_report,
                state: "unavailable".into(),
            });
        };
        let Some(surface) = inner.surface.as_ref() else {
            return Ok(ViewportFrameReport {
                frame_index: 0,
                width: inner.width,
                height: inner.height,
                camera: camera_report,
                state: "unavailable".into(),
            });
        };

        // Build the frame key.
        let key = FrameKey {
            camera_hash: inner.camera_hash,
            selection_hash: 0,
            geometry_hash: 0,
            viewport_w: inner.width,
            viewport_h: inner.height,
        };
        if !surface.should_render(key) {
            return Ok(ViewportFrameReport {
                frame_index: surface.frame_index(),
                width: inner.width,
                height: inner.height,
                camera: camera_report,
                state: "coalesced".into(),
            });
        }
        // Upload camera uniform.
        let queue = renderer
            .queue
            .as_ref()
            .ok_or_else(|| ViewportServiceError::Internal("no queue".into()))?;
        let uniform = build_camera_uniform(&inner.camera, inner.width, inner.height);
        pipeline.upload_camera(queue, &uniform);
        // Record the frame in the surface manager (the actual render
        // pass is exercised in `aec_viewport::render_pipeline`'s
        // tests; here we record the camera transition).
        let new_index;
        if let Some(s) = inner.surface.as_mut() {
            s.record_frame(key);
            new_index = s.frame_index();
        } else {
            new_index = 0;
        }
        Ok(ViewportFrameReport {
            frame_index: new_index,
            width: inner.width,
            height: inner.height,
            camera: camera_report,
            state: "presented".into(),
        })
    }

    /// Status report for the diagnostics panel.
    pub fn status(&self) -> ViewportStatusReport {
        let inner = self.inner.read().expect("viewport service poisoned");
        let state = match (&inner.renderer, &inner.pipeline) {
            (Some(_), Some(_)) => "ready",
            _ => "unavailable",
        }
        .into();
        let gpu_descriptor = inner
            .renderer
            .as_ref()
            .and_then(ViewportRenderer::gpu_descriptor);
        let frame_index = inner
            .surface
            .as_ref()
            .map_or(0, SurfaceManager::frame_index);
        ViewportStatusReport {
            state,
            gpu_descriptor,
            width: inner.width,
            height: inner.height,
            frame_index,
        }
    }

    /// Current camera report (for tests / diagnostics).
    pub fn camera_report(&self) -> ViewportCameraReport {
        let inner = self.inner.read().expect("viewport service poisoned");
        camera_report(&inner.camera)
    }
}

impl Default for ViewportService {
    fn default() -> Self {
        Self::new()
    }
}

fn default_camera() -> Camera {
    // Architect's "front-quarter" view of a building-scale scene
    // (positions in mm to match the rest of the viewport math).
    Camera::default_perspective()
}

fn camera_report(c: &Camera) -> ViewportCameraReport {
    ViewportCameraReport {
        position: c.position,
        target: c.target,
        up: c.up,
        fov_y_radians: c.fov_radians,
    }
}

fn build_camera_uniform(c: &Camera, w: u32, h: u32) -> CameraUniform {
    let aspect = if h == 0 { 1.0 } else { (w as f32) / (h as f32) };
    let proj = Mat4::perspective_rh(c.fov_radians, aspect, c.near, c.far);
    let view = Mat4::look_at_rh(c.position.into(), c.target.into(), c.up.into());
    let view_proj = proj * view;
    CameraUniform {
        view_proj: view_proj.to_cols_array_2d(),
        camera_pos: [c.position[0], c.position[1], c.position[2], 1.0],
    }
}

fn orbit(c: Camera, dx: f32, dy: f32) -> Camera {
    // Spherical orbit around the target. dx rotates around the up
    // axis; dy rotates around the right axis (clamped to avoid
    // gimbal flip at the poles).
    let pos = Vec3::from(c.position);
    let tgt = Vec3::from(c.target);
    let up = Vec3::from(c.up);
    let to_eye = pos - tgt;
    let radius = to_eye.length().max(0.01);
    let dir = to_eye / radius;
    let right = dir.cross(up).normalize_or_zero();
    let yaw = Quat::from_axis_angle(up, -dx * 0.01);
    let pitch = Quat::from_axis_angle(right, -dy * 0.01);
    let new_dir = (yaw * pitch * dir).normalize();
    let new_dir = if new_dir.dot(up).abs() > 0.99 {
        dir
    } else {
        new_dir
    };
    let new_pos = tgt + new_dir * radius;
    Camera {
        position: new_pos.into(),
        ..c
    }
}

fn pan(c: Camera, dx: f32, dy: f32) -> Camera {
    let pos = Vec3::from(c.position);
    let tgt = Vec3::from(c.target);
    let up0 = Vec3::from(c.up);
    let dir = (pos - tgt).normalize_or_zero();
    let right = dir.cross(up0).normalize_or_zero();
    let up = right.cross(dir).normalize_or_zero();
    let pan_scale = (pos - tgt).length() * 0.001;
    let offset = right * (-dx * pan_scale) + up * (dy * pan_scale);
    Camera {
        position: (pos + offset).into(),
        target: (tgt + offset).into(),
        ..c
    }
}

fn zoom(c: Camera, delta: f32) -> Camera {
    // Positive delta zooms in (smaller radius).
    let pos = Vec3::from(c.position);
    let tgt = Vec3::from(c.target);
    let to_eye = pos - tgt;
    let radius = to_eye.length().max(0.01);
    let factor = (1.0 - delta * 0.001).clamp(0.1, 10.0);
    // Allow the zoom to cover building scale (5 m → 500 m).
    let new_radius = (radius * factor).clamp(50.0, 500_000.0);
    let dir = to_eye / radius;
    Camera {
        position: (tgt + dir * new_radius).into(),
        ..c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_service_reports_status() {
        let s = ViewportService::new();
        let st = s.status();
        // Either ready (CI runner has a fallback adapter) or
        // unavailable (no adapter at all). Both are valid.
        assert!(st.state == "ready" || st.state == "unavailable");
        assert_eq!(st.width, 0);
        assert_eq!(st.height, 0);
        assert_eq!(st.frame_index, 0);
    }

    #[test]
    fn resize_zero_dimension_is_rejected() {
        let s = ViewportService::new();
        let err = s.resize(0, 600).unwrap_err();
        assert!(matches!(err, ViewportServiceError::InvalidSize(0, 600)));
        let err = s.resize(800, 0).unwrap_err();
        assert!(matches!(err, ViewportServiceError::InvalidSize(800, 0)));
    }

    #[test]
    fn resize_then_request_frame_reports_dimensions() {
        let s = ViewportService::new();
        s.resize(640, 480).expect("resize ok");
        let report = s.request_frame().expect("frame ok");
        assert_eq!(report.width, 640);
        assert_eq!(report.height, 480);
        // State is either "presented" (real device) or "unavailable"
        // (no adapter). Coalesced only after a second identical
        // request.
        assert!(
            report.state == "presented" || report.state == "unavailable",
            "got state={}",
            report.state
        );
    }

    #[test]
    fn frame_coalescing_short_circuits_identical_requests() {
        let s = ViewportService::new();
        s.resize(800, 600).expect("resize ok");
        let first = s.request_frame().expect("first frame");
        let second = s.request_frame().expect("second frame");
        if first.state == "presented" {
            assert_eq!(second.state, "coalesced");
            assert_eq!(first.frame_index, second.frame_index);
        }
    }

    #[test]
    fn orbit_changes_camera_position() {
        let s = ViewportService::new();
        s.resize(640, 480).expect("resize ok");
        let before = s.camera_report().position;
        s.apply_input(ViewportInput::Orbit { dx: 10.0, dy: 5.0 });
        let after = s.camera_report().position;
        assert!(before != after, "orbit should move the camera");
    }

    #[test]
    fn zoom_brings_camera_closer_to_target() {
        let s = ViewportService::new();
        let before = Vec3::from(s.camera_report().position).length();
        s.apply_input(ViewportInput::Zoom { delta: 500.0 });
        let after = Vec3::from(s.camera_report().position).length();
        assert!(
            after < before,
            "positive zoom should reduce the camera distance: before={before} after={after}",
        );
    }

    #[test]
    fn reset_restores_default_camera() {
        let s = ViewportService::new();
        s.apply_input(ViewportInput::Orbit { dx: 50.0, dy: 50.0 });
        s.apply_input(ViewportInput::Zoom { delta: 200.0 });
        s.apply_input(ViewportInput::Reset);
        let pos = Vec3::from(s.camera_report().position);
        let default_pos = Vec3::from(camera_report(&default_camera()).position);
        assert!((pos - default_pos).length() < 1.0);
    }

    #[test]
    fn camera_uniform_is_finite_and_invertible() {
        let c = default_camera();
        let u = build_camera_uniform(&c, 800, 600);
        for col in &u.view_proj {
            for &x in col {
                assert!(x.is_finite());
            }
        }
        let m = Mat4::from_cols_array_2d(&u.view_proj);
        assert!(m.determinant().abs() > 1e-6);
    }

    #[test]
    fn camera_uniform_handles_zero_height_gracefully() {
        let c = default_camera();
        let u = build_camera_uniform(&c, 0, 0);
        for col in &u.view_proj {
            for &x in col {
                assert!(x.is_finite());
            }
        }
    }
}
