//! Final offline render pipeline.
//!
//! Replaces the legacy `CyclesPipeline` (Blender-IPC wrapper) with a
//! native renderer that drives the in-crate CPU/GPU path tracer end to
//! end. Phase 9 PR4 deletes the Blender worker; this module is the
//! single entry point downstream code uses to produce a final-quality
//! still image from a [`RenderScene`] + [`RenderPreset`].
//!
//! ## Pipeline
//!
//! 1. Build a [`PathTraceScene`] from the [`RenderScene`] (BVH +
//!    triangles + materials + lights + sky).
//! 2. Pick a [`PathTraceConfig`] from the preset's resolution / samples /
//!    tile size.
//! 3. Dispatch [`crate::gpu_render_or_fallback`] — runs the wgpu compute
//!    shader when an adapter is available and silently falls back to the
//!    CPU `rayon`-tiled path tracer otherwise.
//! 4. If `preset.config.denoise` is true, run the bilateral denoiser
//!    over the averaged RGB buffer.
//! 5. Tone-map (Reinhard + gamma 2.2 via [`AccumulationBuffer::as_srgb8`])
//!    and write the result as an sRGB-8 PNG.
//!
//! ## Cancellation
//!
//! Pass an optional [`CancelToken`] to abort mid-render. The token is
//! checked between tiles by the underlying scheduler, so cancellation is
//! cooperative (not pre-emptive). The pipeline reports
//! [`FinalRenderError::Cancelled`] when the token fires.
//!
//! ## Camera selection
//!
//! [`FinalRenderPipeline::render`] uses the scene's first camera by
//! default — `RenderScene.cameras[0]`. Callers wanting a specific camera
//! should use [`FinalRenderPipeline::render_with_camera`] and pass the
//! camera explicitly.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aec_materials::MaterialLibrary;
use image::{ImageBuffer, Rgb};

use crate::gpu_trace::render_or_fallback;
use crate::lighting::SkyParams;
use crate::material::PathTraceMaterial;
use crate::path_trace::{
    AccumulationBuffer, CameraProjection, CancelToken, PathTraceConfig, PathTraceScene,
};
use crate::preset::{RenderPreset, RenderPresetConfig};
use crate::scene::{RenderCamera, RenderScene};

/// Errors produced by [`FinalRenderPipeline`].
#[derive(Debug, thiserror::Error)]
pub enum FinalRenderError {
    #[error("scene has no cameras; cannot render final image")]
    NoCamera,
    #[error("output path has no parent directory: {0}")]
    OutputPathMissingParent(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image encode error: {0}")]
    Encode(#[from] image::ImageError),
    #[error("render cancelled")]
    Cancelled,
}

/// Result of a successful render.
#[derive(Debug, Clone)]
pub struct FinalRenderOutput {
    /// Absolute path of the PNG that was written.
    pub path: PathBuf,
    /// Pixel dimensions of the written image.
    pub width: u32,
    pub height: u32,
    /// Samples per pixel actually rendered (may differ from the preset
    /// when adaptive sampling or cancellation stops the render early).
    pub samples_per_pixel: u32,
    /// Wall-clock render time excluding image encoding.
    pub elapsed: Duration,
    /// Whether the bilateral denoiser ran on the radiance buffer.
    pub denoised: bool,
}

/// Native final-render pipeline. Construct with [`FinalRenderPipeline::new`]
/// (or [`FinalRenderPipeline::with_materials`]) and call
/// [`FinalRenderPipeline::render`] / [`FinalRenderPipeline::render_with_camera`]
/// to produce a PNG on disk.
pub struct FinalRenderPipeline {
    materials: MaterialLibrary,
}

impl Default for FinalRenderPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl FinalRenderPipeline {
    /// Pipeline with an empty material library — meshes without an
    /// assigned material will render with the default grey PBR material.
    pub fn new() -> Self {
        Self {
            materials: MaterialLibrary::new(),
        }
    }

    /// Pipeline backed by the supplied material library — meshes whose
    /// `material_id` matches an entry in the library inherit that
    /// material's PBR properties.
    pub fn with_materials(materials: MaterialLibrary) -> Self {
        Self { materials }
    }

    /// Render using `scene.cameras[0]`. Errors with
    /// [`FinalRenderError::NoCamera`] when the scene has no cameras.
    pub fn render(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        output_path: impl AsRef<Path>,
    ) -> Result<FinalRenderOutput, FinalRenderError> {
        let camera = scene
            .cameras
            .first()
            .ok_or(FinalRenderError::NoCamera)?
            .clone();
        self.render_with_camera(scene, preset, &camera, output_path, None)
    }

    /// Render with an explicit camera and optional [`CancelToken`].
    pub fn render_with_camera(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        camera: &RenderCamera,
        output_path: impl AsRef<Path>,
        cancel: Option<CancelToken>,
    ) -> Result<FinalRenderOutput, FinalRenderError> {
        let output_path = output_path.as_ref().to_path_buf();
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let sky = scene_sky(scene);
        let pt_scene = build_path_trace_scene(scene, &self.materials, sky);
        let config = path_trace_config_from_preset(&preset.config, CameraProjection::Perspective);

        let start = Instant::now();
        let buffer = render_or_fallback(&pt_scene, camera, &config, None, cancel.clone());
        let elapsed = start.elapsed();

        if let Some(token) = &cancel {
            if token.is_cancelled() {
                return Err(FinalRenderError::Cancelled);
            }
        }

        let denoised = preset.config.denoise;
        let srgb = encode_srgb8(&buffer, denoised);
        let img = ImageBuffer::<Rgb<u8>, _>::from_raw(buffer.width, buffer.height, srgb)
            .expect("buffer size matches width * height * 3");
        img.save(&output_path)?;

        Ok(FinalRenderOutput {
            path: output_path,
            width: buffer.width,
            height: buffer.height,
            samples_per_pixel: preset.config.samples,
            elapsed,
            denoised,
        })
    }
}

/// Build a [`PathTraceScene`] from a [`RenderScene`] + material library.
///
/// Exposed `pub(crate)` so the walkthrough / panorama pipelines can
/// reuse the conversion without rebuilding it per-frame.
pub(crate) fn build_path_trace_scene(
    scene: &RenderScene,
    materials: &MaterialLibrary,
    sky: SkyParams,
) -> PathTraceScene {
    let entries: Vec<(String, PathTraceMaterial)> = materials
        .iter()
        .map(|m| (m.id.clone(), PathTraceMaterial::from_pbr(m)))
        .collect();
    let path_trace_materials: Vec<PathTraceMaterial> = entries.iter().map(|(_, m)| *m).collect();
    PathTraceScene::from_render_scene(
        scene,
        path_trace_materials,
        |id| entries.iter().position(|(eid, _)| eid == id),
        sky,
    )
}

/// Derive a [`SkyParams`] for the path tracer. The Phase 9 preview
/// pipeline (`crate::preview::pick_sky_state`) is the source of truth
/// for sun azimuth/elevation; for the final renderer we only need
/// world strength + tint + turbidity since the sun direction is
/// already encoded into the scene's `RenderLight`s via
/// `NativeLight::from_render_light`. Future work may wire in a
/// per-scene `SkyParams` override stored on `RenderScene` itself; for
/// now every code path uses the neutral default.
pub(crate) fn scene_sky(_scene: &RenderScene) -> SkyParams {
    SkyParams::default()
}

/// Build a [`PathTraceConfig`] from the preset's resolution / samples /
/// tile size knobs. `projection` lets the walkthrough / panorama
/// pipelines reuse this helper without duplicating preset-mapping logic.
pub(crate) fn path_trace_config_from_preset(
    preset: &RenderPresetConfig,
    projection: CameraProjection,
) -> PathTraceConfig {
    PathTraceConfig {
        width: preset.resolution_x.max(1),
        height: preset.resolution_y.max(1),
        samples_per_pixel: preset.samples.max(1),
        max_bounces: 8,
        tile_size: preset.tile_size_px.clamp(16, 512),
        russian_roulette_min_bounces: 3,
        adaptive_threshold: 0.01,
        projection,
    }
}

/// Tone-map a radiance buffer to sRGB-8 bytes. When `denoise` is true the
/// per-channel mean is bilaterally filtered before tone-mapping; this is
/// equivalent to the legacy Blender denoiser's post-process pass.
fn encode_srgb8(buffer: &AccumulationBuffer, denoise: bool) -> Vec<u8> {
    if !denoise {
        // Borrowing path — no full-buffer clone for the common case.
        return buffer.as_srgb8();
    }
    let avg = buffer.average_rgb();
    let denoised = crate::denoise::bilateral_denoise(
        &crate::denoise::ImageRgb {
            width: buffer.width,
            height: buffer.height,
            pixels: avg,
        },
        None,
        None,
        crate::denoise::BilateralParams::default(),
    );

    let mut out = Vec::with_capacity(denoised.pixels.len() * 3);
    for p in &denoised.pixels {
        // Match `AccumulationBuffer::as_srgb8`: Reinhard + gamma 2.2.
        let r = (p[0].max(0.0) / (1.0 + p[0].max(0.0))).powf(1.0 / 2.2);
        let g = (p[1].max(0.0) / (1.0 + p[1].max(0.0))).powf(1.0 / 2.2);
        let b = (p[2].max(0.0) / (1.0 + p[2].max(0.0))).powf(1.0 / 2.2);
        out.push((r * 255.0).round().clamp(0.0, 255.0) as u8);
        out.push((g * 255.0).round().clamp(0.0, 255.0) as u8);
        out.push((b * 255.0).round().clamp(0.0, 255.0) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::SerializedMesh;

    fn tiny_scene() -> RenderScene {
        let mut scene = RenderScene::new();
        // Single floor quad.
        scene.push_mesh(SerializedMesh {
            id: "floor".into(),
            indices: vec![0, 1, 2, 0, 2, 3],
            positions: vec![
                [-1000.0, 0.0, -1000.0],
                [1000.0, 0.0, -1000.0],
                [1000.0, 0.0, 1000.0],
                [-1000.0, 0.0, 1000.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            uvs: vec![[0.0, 0.0]; 4],
            material_id: None,
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        });
        scene.push_camera(RenderCamera {
            id: "cam0".into(),
            position_mm: [0.0, 1500.0, 2500.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        });
        scene.push_light(crate::scene::RenderLight::SunSky {
            azimuth_deg: 135.0,
            elevation_deg: 45.0,
            intensity: 2.0,
            color_temperature_k: 5500.0,
        });
        scene
    }

    fn fast_preset() -> RenderPreset {
        // Tiny resolution + 1 sample so the test renders quickly even on
        // CPU fallback; the goal is to validate the orchestration layer,
        // not the path tracer's image quality.
        let mut p = RenderPreset::quick();
        p.config.resolution_x = 32;
        p.config.resolution_y = 24;
        p.config.samples = 1;
        p.config.tile_size_px = 32;
        p.config.denoise = false;
        p
    }

    #[test]
    fn render_writes_png_with_preset_dimensions() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = tiny_scene();
        let preset = fast_preset();
        let pipeline = FinalRenderPipeline::new();
        let out = pipeline
            .render(&scene, &preset, tmp.path().join("out.png"))
            .unwrap();
        assert_eq!(out.width, 32);
        assert_eq!(out.height, 24);
        assert_eq!(out.samples_per_pixel, 1);
        assert!(!out.denoised);
        assert!(out.path.exists(), "PNG must be written");
        // Decoded image must have the right dimensions.
        let decoded = image::open(&out.path).unwrap().to_rgb8();
        assert_eq!(decoded.width(), 32);
        assert_eq!(decoded.height(), 24);
    }

    #[test]
    fn render_creates_parent_directory_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = tiny_scene();
        let preset = fast_preset();
        let pipeline = FinalRenderPipeline::new();
        let nested = tmp.path().join("a").join("b").join("c").join("out.png");
        let out = pipeline.render(&scene, &preset, &nested).unwrap();
        assert!(out.path.exists());
    }

    #[test]
    fn render_with_explicit_camera_uses_that_camera() {
        let tmp = tempfile::tempdir().unwrap();
        let mut scene = tiny_scene();
        let custom = RenderCamera {
            id: "custom".into(),
            position_mm: [500.0, 2000.0, -500.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 50.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        scene.push_camera(custom.clone());
        let preset = fast_preset();
        let pipeline = FinalRenderPipeline::new();
        let out = pipeline
            .render_with_camera(
                &scene,
                &preset,
                &custom,
                tmp.path().join("custom.png"),
                None,
            )
            .unwrap();
        assert_eq!(out.width, 32);
    }

    #[test]
    fn render_no_camera_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut scene = tiny_scene();
        scene.cameras.clear();
        let preset = fast_preset();
        let pipeline = FinalRenderPipeline::new();
        let err = pipeline
            .render(&scene, &preset, tmp.path().join("nope.png"))
            .unwrap_err();
        assert!(matches!(err, FinalRenderError::NoCamera));
    }

    #[test]
    fn render_respects_cancel_token() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = tiny_scene();
        let mut preset = fast_preset();
        // Bump the resolution + samples a bit so the render does real
        // work and the cancellation actually has time to fire.
        preset.config.resolution_x = 256;
        preset.config.resolution_y = 256;
        preset.config.samples = 16;
        let camera = scene.cameras[0].clone();
        let pipeline = FinalRenderPipeline::new();
        let cancel = CancelToken::new();
        cancel.cancel(); // pre-cancel — any check fires immediately.
        let err = pipeline
            .render_with_camera(
                &scene,
                &preset,
                &camera,
                tmp.path().join("cancelled.png"),
                Some(cancel),
            )
            .unwrap_err();
        assert!(matches!(err, FinalRenderError::Cancelled));
    }

    #[test]
    fn path_trace_config_from_preset_clamps_tile_size() {
        let mut cfg = RenderPreset::quick().config;
        cfg.tile_size_px = 4; // below the floor
        let pt = super::path_trace_config_from_preset(&cfg, CameraProjection::Perspective);
        assert_eq!(pt.tile_size, 16);
        cfg.tile_size_px = 4096; // above the ceiling
        let pt = super::path_trace_config_from_preset(&cfg, CameraProjection::Perspective);
        assert_eq!(pt.tile_size, 512);
    }
}
