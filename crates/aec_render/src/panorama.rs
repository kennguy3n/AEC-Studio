//! Native 360° equirectangular panorama renderer.
//!
//! Replaces the legacy `workers/blender/panorama.py` worker. Uses the
//! native path tracer's [`CameraProjection::Equirectangular`] primary
//! ray generator (see [`crate::path_trace::PathTraceConfig::panorama`])
//! so the same BVH, materials, lights, and sky model produce the
//! panorama as drive the still and walkthrough renders.
//!
//! Output is a tone-mapped sRGB-8 PNG with a 2:1 aspect ratio. Width and
//! height come from the preset; if the supplied resolution is not 2:1 we
//! force the height to half the width so each pixel covers the same
//! solid angle and consumers can pipe the result directly into a 360°
//! photo viewer without skewing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aec_materials::MaterialLibrary;
use image::{ImageBuffer, Rgb};

use crate::final_render::{
    build_path_trace_scene, encode_srgb8, path_trace_config_from_preset, scene_sky,
};
use crate::gpu_trace::render_or_fallback;
use crate::path_trace::{CameraProjection, CancelToken};
use crate::preset::RenderPreset;
use crate::scene::{RenderCamera, RenderScene};

/// Errors produced by [`PanoramaPipeline::render`].
#[derive(Debug, thiserror::Error)]
pub enum PanoramaError {
    #[error("scene has no cameras; cannot render panorama")]
    NoCamera,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image encode error: {0}")]
    Encode(#[from] image::ImageError),
    #[error("panorama render cancelled")]
    Cancelled,
}

/// Successful panorama render result.
#[derive(Debug, Clone)]
pub struct PanoramaOutput {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub samples_per_pixel: u32,
    pub elapsed: Duration,
    /// Whether the bilateral denoiser ran on the radiance buffer.
    /// Mirrors [`crate::final_render::FinalRenderOutput::denoised`].
    pub denoised: bool,
}

/// Native equirectangular panorama pipeline.
pub struct PanoramaPipeline {
    materials: MaterialLibrary,
}

impl Default for PanoramaPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl PanoramaPipeline {
    pub fn new() -> Self {
        Self {
            materials: MaterialLibrary::new(),
        }
    }

    pub fn with_materials(materials: MaterialLibrary) -> Self {
        Self { materials }
    }

    /// Render a 360° equirectangular panorama from
    /// `scene.cameras[0]`'s position. The camera's `target_mm` controls
    /// where the centre column of the panorama looks (longitude 0).
    pub fn render(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        output_path: impl AsRef<Path>,
    ) -> Result<PanoramaOutput, PanoramaError> {
        let camera = scene
            .cameras
            .first()
            .ok_or(PanoramaError::NoCamera)?
            .clone();
        self.render_with_camera(scene, preset, &camera, output_path, None)
    }

    /// Render with an explicit camera and optional cancellation.
    pub fn render_with_camera(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        camera: &RenderCamera,
        output_path: impl AsRef<Path>,
        cancel: Option<CancelToken>,
    ) -> Result<PanoramaOutput, PanoramaError> {
        let output_path = output_path.as_ref().to_path_buf();
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let sky = scene_sky(scene);
        let pt_scene = build_path_trace_scene(scene, &self.materials, sky);

        // Force 2:1 aspect ratio so each pixel covers equal solid angle.
        // Equirectangular panoramas REQUIRE width = 2 * height exactly,
        // otherwise the bottom row covers a slightly different solid
        // angle than the rest, breaking VR / 360° viewer conventions.
        // Integer division on odd widths (e.g. 65) silently produces an
        // off-by-one height (32, giving 65:32 = 2.03:1) so clamp width
        // down to the nearest even value before deriving height. Stock
        // presets ship even widths (1024, 2048, 4096), but a caller
        // wiring a custom resolution_x should not be able to break the
        // invariant.
        let mut config =
            path_trace_config_from_preset(&preset.config, CameraProjection::Equirectangular);
        let width = (config.width.max(2)) & !1u32;
        // height = width / 2 is now exact (no truncation).
        let height = width / 2;
        config.width = width;
        config.height = height;

        let start = Instant::now();
        // Panorama / equirectangular path: aux feature buffers (albedo /
        // normal / depth) are not currently used for denoising of
        // panoramas because the equirectangular projection produces
        // strong pole distortion that the bilateral kernel handles
        // best with luminance-only filtering. If denoising of panoramas
        // becomes critical, a future PR can opt this path into aux too.
        let buffer = render_or_fallback(&pt_scene, camera, &config, None, cancel.clone(), false);
        let elapsed = start.elapsed();
        if let Some(token) = &cancel {
            if token.is_cancelled() {
                return Err(PanoramaError::Cancelled);
            }
        }

        let buf_width = buffer.width;
        let buf_height = buffer.height;
        // Honour `preset.config.denoise`: panorama presets ship with
        // `denoise: true` by default, so calling `buffer.into_srgb8()`
        // directly would silently produce noisier output than the
        // preset promises. Route through `encode_srgb8` so still,
        // panorama, and walkthrough renders all share one tone-map
        // path and one denoise gate.
        let denoised = preset.config.denoise;
        let srgb = encode_srgb8(&buffer, denoised);
        let img = ImageBuffer::<Rgb<u8>, _>::from_raw(buf_width, buf_height, srgb)
            .expect("buffer size matches width * height * 3");
        img.save(&output_path)?;

        Ok(PanoramaOutput {
            path: output_path,
            width: buf_width,
            height: buf_height,
            samples_per_pixel: preset.config.samples,
            elapsed,
            denoised,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_trace::{
        render_tile_pass, AccumulationBuffer, PathTraceConfig, PathTraceScene, Tile,
    };
    use crate::scene::SerializedMesh;
    use glam::Vec3;

    fn tiny_scene() -> RenderScene {
        let mut scene = RenderScene::new();
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
            position_mm: [0.0, 1500.0, 0.0],
            target_mm: [0.0, 1500.0, -1000.0],
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
        let mut p = RenderPreset::panorama();
        p.config.resolution_x = 64;
        p.config.resolution_y = 32;
        p.config.samples = 1;
        p.config.tile_size_px = 16;
        p.config.denoise = false;
        p
    }

    #[test]
    fn render_produces_two_to_one_panorama_png() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = PanoramaPipeline::new();
        let out = pipeline
            .render(&tiny_scene(), &fast_preset(), tmp.path().join("pano.png"))
            .unwrap();
        assert_eq!(out.width, 64);
        assert_eq!(out.height, 32, "panorama must be 2:1 aspect ratio");
        assert!(out.path.exists());
        let decoded = image::open(&out.path).unwrap().to_rgb8();
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 32);
    }

    #[test]
    fn equirectangular_ray_generation_covers_full_sphere() {
        // Build a scene with a sun positioned at a specific azimuth so
        // that the equirectangular rays produce a bright spot at the
        // longitude that "looks back" at the sun. Default sky alone is
        // flat radiance in every direction — the variance signal must
        // come from at least one analytic light whose visibility is
        // direction-dependent.
        let mut scene = PathTraceScene {
            bvh: crate::bvh::Bvh::build(&[]),
            triangles: Vec::new(),
            shading_data: Vec::new(),
            materials: vec![crate::material::PathTraceMaterial::default_grey()],
            material_ids: Vec::new(),
            lights: Vec::new(),
            sky: crate::lighting::SkyParams::default(),
        };
        // A wide sun (15° half-angle ≈ 0.26 rad) so it occupies several
        // pixels of a low-resolution panorama; small enough that not
        // every pixel sees it. Pointing straight at +X (i.e. the sun
        // sits to the camera's right) so the bright spot is on the
        // right-half of the panorama.
        scene.lights.push(crate::light_sampling::NativeLight::Sun {
            direction: Vec3::new(-1.0, 0.0, 0.0).normalize(),
            radiance: Vec3::splat(50.0),
            angular_radius_rad: 0.26,
        });
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let config = PathTraceConfig {
            width: 16,
            height: 8,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 16,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: CameraProjection::Equirectangular,
        };
        let tile = Tile {
            x_start: 0,
            y_start: 0,
            x_end: config.width,
            y_end: config.height,
        };
        let result = render_tile_pass(&scene, &camera, &config, tile, 4, 0, 0xC0FFEE);
        assert_eq!(result.sums.len() as u32, config.width * config.height);
        for px in &result.sums {
            // Every pixel must produce *some* radiance — at minimum the
            // default sky contributes 0.5 per channel.
            assert!(
                px[0] > 0.0 || px[1] > 0.0 || px[2] > 0.0,
                "expected sky radiance from equirectangular sample"
            );
        }
        // The sun makes some pixels much brighter than others, so the
        // average radiance must have non-zero variance across the
        // panorama. This is the canonical check that ray generation
        // actually produced distinct world-space directions per pixel.
        let mut buffer = AccumulationBuffer::new(config.width, config.height);
        for (i, px) in result.sums.iter().enumerate() {
            buffer.pixels[i] = [px[0], px[1], px[2], px[3]];
        }
        let avg = buffer.average_rgb();
        let mut mean = [0.0_f32; 3];
        for p in &avg {
            for (m, v) in mean.iter_mut().zip(p.iter()) {
                *m += *v;
            }
        }
        for m in &mut mean {
            *m /= avg.len() as f32;
        }
        let mut var_sum = 0.0_f32;
        for p in &avg {
            for (m, v) in mean.iter().zip(p.iter()) {
                let d = *v - *m;
                var_sum += d * d;
            }
        }
        assert!(
            var_sum > 0.0,
            "panorama must have non-zero variance when a sun is visible"
        );
    }

    #[test]
    fn render_no_camera_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut scene = tiny_scene();
        scene.cameras.clear();
        let pipeline = PanoramaPipeline::new();
        let err = pipeline
            .render(&scene, &fast_preset(), tmp.path().join("nope.png"))
            .unwrap_err();
        assert!(matches!(err, PanoramaError::NoCamera));
    }

    #[test]
    fn render_respects_cancel_token() {
        let tmp = tempfile::tempdir().unwrap();
        let mut preset = fast_preset();
        preset.config.resolution_x = 256;
        preset.config.samples = 4;
        let scene = tiny_scene();
        let camera = scene.cameras[0].clone();
        let pipeline = PanoramaPipeline::new();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = pipeline
            .render_with_camera(
                &scene,
                &preset,
                &camera,
                tmp.path().join("c.png"),
                Some(cancel),
            )
            .unwrap_err();
        assert!(matches!(err, PanoramaError::Cancelled));
    }

    #[test]
    fn render_forces_two_to_one_aspect_even_when_preset_isnt() {
        let tmp = tempfile::tempdir().unwrap();
        let mut preset = fast_preset();
        // Square preset; pipeline must override height to width / 2.
        preset.config.resolution_x = 128;
        preset.config.resolution_y = 128;
        let pipeline = PanoramaPipeline::new();
        let out = pipeline
            .render(&tiny_scene(), &preset, tmp.path().join("p.png"))
            .unwrap();
        assert_eq!(out.width, 128);
        assert_eq!(out.height, 64);
    }

    #[test]
    fn render_clamps_odd_width_to_even_to_preserve_two_to_one_aspect() {
        // Odd preset width must be clamped down to the nearest even
        // value so the equirectangular `width = 2 * height` invariant
        // holds exactly. Without the clamp, 65 px width would produce
        // 65 x 32 = 2.03:1 aspect, breaking the constant solid-angle
        // assumption made by every downstream 360° viewer.
        let tmp = tempfile::tempdir().unwrap();
        let mut preset = fast_preset();
        preset.config.resolution_x = 65;
        preset.config.resolution_y = 65;
        let pipeline = PanoramaPipeline::new();
        let out = pipeline
            .render(&tiny_scene(), &preset, tmp.path().join("p_odd.png"))
            .unwrap();
        assert_eq!(out.width, 64);
        assert_eq!(out.height, 32);
        assert_eq!(out.width, 2 * out.height);
    }

    #[test]
    fn equirectangular_dir_test_via_two_distinct_pixels() {
        // Verify two different pixels yield different world-space
        // directions. Render a panorama where the sun is large enough
        // that stratified jitter will see it at the equator but not at
        // the zenith, and check that the two regions differ in radiance.
        //
        // (We deliberately do *not* compare bit-identical pixel sums
        // here: with low-discrepancy jitter [stratified Halton(2,3)],
        // every pixel's per-sample directions cluster on the same
        // unit-square Halton points, so two pixels seeing the same
        // uniform sky background reliably produce identical radiance.
        // The brittleness of an exact-sum comparison was a property of
        // the prior pure-random ray-gen, not a feature.)
        let mut scene = PathTraceScene {
            bvh: crate::bvh::Bvh::build(&[]),
            triangles: Vec::new(),
            shading_data: Vec::new(),
            materials: vec![crate::material::PathTraceMaterial::default_grey()],
            material_ids: Vec::new(),
            lights: Vec::new(),
            sky: crate::lighting::SkyParams::default(),
        };
        // Large sun (~17° angular radius) so even a 2-sample pixel at
        // the equator reliably hits it; tiny suns are too brittle.
        scene.lights.push(crate::light_sampling::NativeLight::Sun {
            direction: Vec3::new(0.0, -1.0, 0.0).normalize(),
            radiance: Vec3::splat(10.0),
            angular_radius_rad: 0.3,
        });
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let config = PathTraceConfig {
            width: 8,
            height: 4,
            samples_per_pixel: 4,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: CameraProjection::Equirectangular,
        };
        let tile = Tile {
            x_start: 0,
            y_start: 0,
            x_end: 8,
            y_end: 4,
        };
        let result = render_tile_pass(&scene, &camera, &config, tile, 4, 0, 0xBEEF);
        // The panorama mapping at v=0 (top row) points toward +Y; sun
        // direction is `Vec3::new(0, -1, 0)` so the apparent sun is at
        // +Y, which lands on the top row. The top row must therefore
        // be measurably brighter than the bottom row, proving (a)
        // pixel coordinates affect ray direction and (b) the mapping
        // is the documented centre-forward convention.
        let top_row_lum: f32 = (0..8)
            .map(|x| result.sums[x][0] + result.sums[x][1] + result.sums[x][2])
            .sum();
        let bottom_row_lum: f32 = (0..8)
            .map(|x| {
                let i = 3 * 8 + x;
                result.sums[i][0] + result.sums[i][1] + result.sums[i][2]
            })
            .sum();
        assert!(
            top_row_lum > bottom_row_lum + 1.0,
            "equirectangular pixels must vary across image: top {top_row_lum}, bottom {bottom_row_lum}"
        );
    }
}
