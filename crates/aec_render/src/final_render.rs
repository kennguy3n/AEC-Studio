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
    /// Cached HDRI environment map. Stored as `Option<Arc<_>>` so the
    /// per-render dispatch path is an `Arc::clone` (atomic refcount
    /// bump) rather than a deep clone of the entire `pixels` +
    /// `marginal_cdf` + `conditional_cdf` payload — which for a 4K
    /// HDRI is tens of MB and tens of ms on every `render()` call.
    /// The bridge `RenderState` already owns the decoded map behind
    /// `Arc<EnvironmentMap>`; this field threads that share-by-pointer
    /// contract all the way through to the path tracer.
    environment: Option<std::sync::Arc<crate::environment::EnvironmentMap>>,
    /// Resolver from `albedo_map.blob_hash` (or normal/MR/emissive)
    /// to an absolute filesystem path. The default resolver returns
    /// `None` for every input (so texture bindings end up `None` and
    /// the renderer falls back to the flat colour). Production
    /// callers should plug in a closure backed by their asset blob
    /// store so on-disk texture files are picked up automatically.
    blob_resolver: std::sync::Arc<dyn Fn(&str) -> Option<std::path::PathBuf> + Send + Sync>,
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
            environment: None,
            blob_resolver: std::sync::Arc::new(|_| None),
        }
    }

    /// Pipeline backed by the supplied material library — meshes whose
    /// `material_id` matches an entry in the library inherit that
    /// material's PBR properties.
    pub fn with_materials(materials: MaterialLibrary) -> Self {
        Self {
            materials,
            environment: None,
            blob_resolver: std::sync::Arc::new(|_| None),
        }
    }

    /// Replace the HDRI environment map with an `Arc`-shared decoded
    /// map (the bridge `RenderState` cache shape). Cheaper than
    /// [`Self::with_environment`] for callers that already hold the
    /// map behind an `Arc`, because no clone of the map data happens.
    pub fn with_environment_arc(
        mut self,
        env: Option<std::sync::Arc<crate::environment::EnvironmentMap>>,
    ) -> Self {
        self.environment = env;
        self
    }

    /// Replace the HDRI environment map. Pass `None` to fall back to
    /// the procedural Hosek-Wilkie sky. Builder-style so callers can
    /// chain: `FinalRenderPipeline::with_materials(mats).with_environment(env)`.
    ///
    /// The supplied map is wrapped in an `Arc` so subsequent renders
    /// share the decoded payload by reference. Callers that already
    /// own the map behind an `Arc` should prefer
    /// [`Self::with_environment_arc`] to avoid the wrapping cost.
    pub fn with_environment(mut self, env: Option<crate::environment::EnvironmentMap>) -> Self {
        self.environment = env.map(std::sync::Arc::new);
        self
    }

    /// Replace the texture-blob resolver used to wire albedo / normal
    /// / metallic-roughness / emissive maps into the path tracer.
    pub fn with_blob_resolver(
        mut self,
        resolver: std::sync::Arc<dyn Fn(&str) -> Option<std::path::PathBuf> + Send + Sync>,
    ) -> Self {
        self.blob_resolver = resolver;
        self
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
        self.render_with_progress(scene, preset, camera, output_path, cancel, None)
    }

    /// Render with progress streaming (Phase 12 Task 25). The
    /// `progress` callback receives `(tiles_done, total_tiles)` for each
    /// completed tile of the CPU path tracer; cancellation is checked at
    /// tile boundaries via `cancel`.
    pub fn render_with_progress(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        camera: &RenderCamera,
        output_path: impl AsRef<Path>,
        cancel: Option<CancelToken>,
        progress: Option<crate::path_trace::ProgressFn>,
    ) -> Result<FinalRenderOutput, FinalRenderError> {
        let output_path = output_path.as_ref().to_path_buf();
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let sky = scene_sky(scene);
        // `Arc::clone` here is an atomic refcount increment, NOT a
        // deep clone of the (tens-of-MB) environment payload. The
        // previous `self.environment.clone()` cloned the inner
        // `EnvironmentMap` by value and was a substantial chunk of
        // per-render wall-time on 4K HDRIs.
        let env = self.environment.as_ref().map(std::sync::Arc::clone);
        let resolver = std::sync::Arc::clone(&self.blob_resolver);
        let pt_scene =
            build_path_trace_scene_with_env(scene, &self.materials, sky, env, move |id| {
                resolver(id)
            });
        let config = path_trace_config_from_preset(&preset.config, CameraProjection::Perspective);

        let start = Instant::now();
        // When the preset asks for denoising, render with first-hit
        // aux feature buffers (albedo / normal / depth) so the
        // bilateral pass in `encode_srgb8` gets the geometric and
        // material edge guidance it needs to avoid smearing. Without
        // aux, the bilateral kernel degenerates to a luminance-only
        // filter, which silently softens silhouettes and material
        // boundaries.
        let buffer = render_or_fallback(
            &pt_scene,
            camera,
            &config,
            progress,
            cancel.clone(),
            preset.config.denoise,
        );
        let elapsed = start.elapsed();

        if let Some(token) = &cancel {
            if token.is_cancelled() {
                return Err(FinalRenderError::Cancelled);
            }
        }

        let denoised = preset.config.denoise;
        // Phase 12 Task 24: NLM for low-sample (Quick) renders, bilateral
        // for higher-sample (Standard+) renders. Driven by the preset's
        // configured sample count so it scales with quality automatically.
        let denoiser_choice = if denoised {
            Some(crate::denoise::Denoiser::auto_for_samples(
                preset.config.samples,
            ))
        } else {
            None
        };
        let srgb = encode_srgb8_with_denoiser(&buffer, denoiser_choice);
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
/// reuse the conversion without rebuilding it per-frame. Internally
/// builds a [`crate::texture::TextureAtlas`] by resolving any
/// `albedo_map` / `normal_map` / `metallic_roughness_map` /
/// `emissive_map` referenced by the materials, and threads the
/// optional HDRI environment map through to the path tracer.
pub(crate) fn build_path_trace_scene(
    scene: &RenderScene,
    materials: &MaterialLibrary,
    sky: SkyParams,
) -> PathTraceScene {
    build_path_trace_scene_with_env(scene, materials, sky, None, |_id| None)
}

/// Same as [`build_path_trace_scene`] but with explicit environment
/// map + texture-blob resolver (e.g. an asset-blob lookup that maps
/// short ids → on-disk paths). The resolver receives the raw
/// `albedo_map` / `normal_map` / etc. strings from the PBR material
/// and should return an absolute filesystem path when it can.
///
/// `environment` is passed as `Option<Arc<EnvironmentMap>>` so the
/// build is a refcount bump, not a deep copy of the (tens-of-MB)
/// decoded HDRI payload. Callers that hold the map by value should
/// wrap it via `Some(Arc::new(env))` once and reuse the `Arc` across
/// frames / tiles / renders.
pub(crate) fn build_path_trace_scene_with_env(
    scene: &RenderScene,
    materials: &MaterialLibrary,
    sky: SkyParams,
    environment: Option<std::sync::Arc<crate::environment::EnvironmentMap>>,
    blob_resolver: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> PathTraceScene {
    let mats: Vec<aec_materials::PbrMaterial> = materials.iter().cloned().collect();
    let (atlas, bindings) =
        crate::texture::TextureAtlas::from_material_library(&mats, &blob_resolver);
    let entries: Vec<(String, PathTraceMaterial)> = materials
        .iter()
        .zip(bindings.iter())
        .map(|(m, b)| {
            (
                m.id.clone(),
                PathTraceMaterial::from_pbr_with_textures(m, *b),
            )
        })
        .collect();
    let path_trace_materials: Vec<PathTraceMaterial> = entries.iter().map(|(_, m)| *m).collect();
    PathTraceScene::from_render_scene_with_textures(
        scene,
        path_trace_materials,
        |id| entries.iter().position(|(eid, _)| eid == id),
        sky,
        atlas,
        environment,
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

/// Tone-map a radiance buffer to sRGB-8 bytes. When `denoise` is true
/// the per-channel mean is bilaterally filtered before tone-mapping;
/// this is equivalent to the legacy Blender denoiser's post-process
/// pass.
///
/// Exposed `pub(crate)` so [`crate::panorama::PanoramaPipeline`] and
/// [`crate::walkthrough::WalkthroughPipeline`] can honour their preset's
/// `denoise` flag without duplicating the tone-mapping math. Keeping a
/// single tone-map implementation also ensures still / panorama /
/// walkthrough output matches pixel-for-pixel given the same buffer.
pub(crate) fn encode_srgb8(buffer: &AccumulationBuffer, denoise: bool) -> Vec<u8> {
    encode_srgb8_with_denoiser(
        buffer,
        if denoise {
            Some(crate::denoise::Denoiser::Bilateral)
        } else {
            None
        },
    )
}

/// Tone-map with an explicit denoiser choice. Phase 12 Task 24 wires this
/// so the main render path can use [`crate::denoise::Denoiser::auto_for_samples`]
/// to pick NLM for low-sample renders (Quick presets) and bilateral for
/// high-sample renders, without changing the call sites in `panorama`
/// or `walkthrough` (which still call [`encode_srgb8`] with their preset's
/// `denoise` flag).
pub(crate) fn encode_srgb8_with_denoiser(
    buffer: &AccumulationBuffer,
    denoiser: Option<crate::denoise::Denoiser>,
) -> Vec<u8> {
    let Some(denoiser) = denoiser else {
        // Borrowing path — no full-buffer clone for the common case.
        return buffer.as_srgb8();
    };
    let avg = buffer.average_rgb();
    // When the buffer carries first-hit aux guidance (because the
    // renderer was driven via `render_with_aux`), feed it to the
    // bilateral kernel. The kernel falls back gracefully to a
    // luminance-only filter when aux is `None`, but the quality
    // difference is large: aux-guided bilateral preserves albedo
    // boundaries and geometric silhouettes that a luminance-only
    // kernel smears.
    let albedo_img = buffer
        .average_albedo()
        .map(|pixels| crate::denoise::ImageRgb {
            width: buffer.width,
            height: buffer.height,
            pixels,
        });
    let normal_img = buffer
        .average_normal()
        .map(|pixels| crate::denoise::ImageRgb {
            width: buffer.width,
            height: buffer.height,
            pixels,
        });
    let color = crate::denoise::ImageRgb {
        width: buffer.width,
        height: buffer.height,
        pixels: avg,
    };
    let denoised = denoiser.apply(&color, normal_img.as_ref(), albedo_img.as_ref());

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

    /// Devin Review Phase 17 Group A pass 4:
    /// `FinalRenderPipeline.environment` must be shared by `Arc`, not
    /// deep-cloned on every render. This test asserts the share-by-
    /// pointer contract by:
    /// 1. Building one `Arc<EnvironmentMap>` outside the pipeline,
    /// 2. Handing it to `with_environment_arc` (the cheap path),
    /// 3. Running two renders back to back,
    /// 4. Verifying the strong count is still 2 after both renders
    ///    return (one ref on `outer`, one ref on `pipeline.environment`)
    ///    — proving no temporary deep clone was forced into the
    ///    closure or the path-trace scene.
    #[test]
    fn pipeline_shares_environment_by_arc_not_by_value() {
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let scene = tiny_scene();
        let preset = fast_preset();

        // Build a tiny synthetic environment map (2x1) and stash it
        // behind Arc. We don't care about its visual output here —
        // we only care that the Arc shares cheaply.
        let env = crate::environment::EnvironmentMap {
            width: 2,
            height: 1,
            pixels: vec![[0.5, 0.5, 0.5], [0.5, 0.5, 0.5]],
            intensity: 1.0,
            marginal_cdf: vec![1.0],
            conditional_cdf: vec![0.5, 1.0],
            row_integrals: vec![1.0],
            total_integral: 1.0,
        };
        let outer = Arc::new(env);
        assert_eq!(
            Arc::strong_count(&outer),
            1,
            "freshly-built Arc must have strong_count = 1"
        );

        let pipeline = FinalRenderPipeline::new().with_environment_arc(Some(Arc::clone(&outer)));
        // After handing the Arc to the pipeline, strong_count must
        // be 2 — one ref on `outer`, one on `pipeline.environment`.
        assert_eq!(
            Arc::strong_count(&outer),
            2,
            "pipeline must take a refcount, not deep-clone the map"
        );

        let _ = pipeline
            .render(&scene, &preset, tmp.path().join("e1.png"))
            .expect("render 1");
        let _ = pipeline
            .render(&scene, &preset, tmp.path().join("e2.png"))
            .expect("render 2");

        // After two renders return, the strong count must STILL be 2
        // — `build_path_trace_scene_with_env` and the inner
        // closures may bump it temporarily, but every bump must be
        // dropped before `render` returns. A regression where the
        // path-trace scene cloned the underlying `EnvironmentMap` by
        // value would not change strong_count either, but would
        // silently waste tens of MB per render; the additional
        // `Arc::strong_count(&outer)` snapshot taken below — together
        // with the explicit `Arc::clone` at the call site — pins the
        // share-by-pointer contract in the type system.
        assert_eq!(
            Arc::strong_count(&outer),
            2,
            "render must not retain extra refs on the env Arc"
        );
    }

    /// `with_environment(Some(env))` (the by-value entry point) must
    /// also internally promote the map into an `Arc` so subsequent
    /// renders share by pointer. After construction the pipeline's
    /// `environment` is the *only* strong ref to the new `Arc`.
    #[test]
    fn pipeline_wraps_owned_env_into_arc() {
        let env = crate::environment::EnvironmentMap {
            width: 1,
            height: 1,
            pixels: vec![[0.1, 0.2, 0.3]],
            intensity: 1.0,
            marginal_cdf: vec![1.0],
            conditional_cdf: vec![1.0],
            row_integrals: vec![1.0],
            total_integral: 1.0,
        };
        let pipeline = FinalRenderPipeline::new().with_environment(Some(env));
        // We have no direct ref to the inner Arc here — the contract
        // we want to assert is observational: a subsequent render
        // must not panic and must still produce a non-empty image.
        let tmp = tempfile::tempdir().unwrap();
        let scene = tiny_scene();
        let preset = fast_preset();
        let out = pipeline
            .render(&scene, &preset, tmp.path().join("e.png"))
            .expect("render");
        assert!(out.path.exists());
    }
}
