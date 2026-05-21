// Single-letter math identifiers (m, p, n, w, t, u, v, x, y, z) match the
// standard path tracing notation; expanding them hurts readability of the
// kernel.
#![allow(clippy::many_single_char_names)]

//! CPU path tracer.
//!
//! Native Rust replacement for the megakernel path-tracing loop in
//! Cycles (`src/integrator/path_trace_work_cpu.cpp`,
//! `src/kernel/integrator/megakernel.h`). Implements:
//!
//! * Camera ray generation through a pinhole + thin-lens camera.
//! * BVH traversal via [`crate::intersect::closest_hit`].
//! * Material evaluation + importance-sampling via [`crate::material`].
//! * Direct lighting with next-event estimation, MIS-combined with the
//!   BSDF sample.
//! * Russian roulette termination after bounce 3.
//! * Tile-based multi-threaded rendering with Rayon.
//! * Cancellation via [`std::sync::atomic::AtomicBool`].
//!
//! The path tracer reads scene data assembled by
//! [`PathTraceScene::from_render_scene`]: triangles, per-triangle shading
//! info, materials, lights, camera, and sky.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use glam::{Mat3, Vec3};
use rayon::prelude::*;

use crate::bvh::{BuilderTriangle, Bvh};
use crate::intersect::{any_hit, closest_hit, geom_normal, Ray, ShadingTriangle};
use crate::light_sampling::{
    environment_radiance, is_delta, power_heuristic, sample_light, NativeLight,
};
use crate::lighting::SkyParams;
use crate::material::{eval_bsdf, pdf_bsdf, sample_bsdf, PathTraceMaterial};
use crate::scene::{RenderCamera, RenderScene};

/// Scene compiled into the format the path tracer consumes.
pub struct PathTraceScene {
    pub bvh: Bvh,
    pub triangles: Vec<ShadingTriangle>,
    /// Per-triangle interpolated-normal data + UVs. Same length as
    /// [`Self::triangles`].
    pub shading_data: Vec<TriangleShading>,
    pub materials: Vec<PathTraceMaterial>,
    /// Mapping from triangle index → material index in `materials`. -1 if
    /// no material is assigned (use the default grey).
    pub material_ids: Vec<i32>,
    pub lights: Vec<NativeLight>,
    pub sky: SkyParams,
}

/// Per-triangle data needed for shading (smooth normals, UVs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriangleShading {
    pub n0: Vec3,
    pub n1: Vec3,
    pub n2: Vec3,
}

impl PathTraceScene {
    /// Build a [`PathTraceScene`] from the existing [`RenderScene`] +
    /// material library + sky parameters.
    pub fn from_render_scene(
        scene: &RenderScene,
        materials: Vec<PathTraceMaterial>,
        material_lookup: impl Fn(&str) -> Option<usize>,
        sky: SkyParams,
    ) -> Self {
        let mut triangles: Vec<ShadingTriangle> = Vec::new();
        let mut shading: Vec<TriangleShading> = Vec::new();
        let mut material_ids: Vec<i32> = Vec::new();
        let mut builder: Vec<BuilderTriangle> = Vec::new();

        for (mesh_idx, mesh) in scene.meshes.iter().enumerate() {
            // Apply the mesh transform to vertex positions and normals.
            // Mesh transform is 4x4 row-major.
            let m = matrix_from_array(mesh.transform);
            let normal_mat = normal_matrix(&m);
            let positions: Vec<Vec3> = mesh
                .positions
                .iter()
                .map(|p| {
                    let v = Vec3::from_array(*p);
                    transform_point(&m, v)
                })
                .collect();
            let normals: Vec<Vec3> = mesh
                .normals
                .iter()
                .map(|n| (normal_mat * Vec3::from_array(*n)).normalize())
                .collect();
            let mat_id = mesh
                .material_id
                .as_ref()
                .and_then(|id| material_lookup(id))
                .map_or(-1, |i| i as i32);
            let n_tris = mesh.indices.len() / 3;
            for tri_idx in 0..n_tris {
                let i0 = mesh.indices[tri_idx * 3] as usize;
                let i1 = mesh.indices[tri_idx * 3 + 1] as usize;
                let i2 = mesh.indices[tri_idx * 3 + 2] as usize;
                if i0 >= positions.len() || i1 >= positions.len() || i2 >= positions.len() {
                    continue;
                }
                let v0 = positions[i0];
                let v1 = positions[i1];
                let v2 = positions[i2];
                let prim_id = triangles.len() as u32;
                triangles.push(ShadingTriangle {
                    v0,
                    v1,
                    v2,
                    prim_id,
                    object_id: mesh_idx as u32,
                });
                builder.push(BuilderTriangle {
                    v0,
                    v1,
                    v2,
                    prim_id,
                });
                let (n0, n1, n2) = if normals.is_empty() {
                    let g = (v1 - v0).cross(v2 - v0).normalize_or_zero();
                    (g, g, g)
                } else {
                    let g0 = *normals.get(i0).unwrap_or(&Vec3::Z);
                    let g1 = *normals.get(i1).unwrap_or(&Vec3::Z);
                    let g2 = *normals.get(i2).unwrap_or(&Vec3::Z);
                    (g0, g1, g2)
                };
                shading.push(TriangleShading { n0, n1, n2 });
                material_ids.push(mat_id);
            }
        }

        let bvh = Bvh::build(&builder);
        let lights: Vec<NativeLight> = scene
            .lights
            .iter()
            .map(NativeLight::from_render_light)
            .collect();

        Self {
            bvh,
            triangles,
            shading_data: shading,
            material_ids,
            materials,
            lights,
            sky,
        }
    }

    pub fn material_for(&self, prim_id: u32) -> PathTraceMaterial {
        let idx = self
            .material_ids
            .get(prim_id as usize)
            .copied()
            .unwrap_or(-1);
        if idx < 0 {
            PathTraceMaterial::default_grey()
        } else {
            self.materials[idx as usize]
        }
    }
}

/// Camera projection used by the path tracer when generating primary
/// rays. `Perspective` is the default; `Equirectangular` is used by the
/// panorama renderer to produce 360°×180° output where horizontal pixels
/// span longitude `[0, 2π]` and vertical pixels span latitude `[0, π]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraProjection {
    #[default]
    Perspective,
    Equirectangular,
}

/// Path-tracer configuration. Most fields map 1:1 to
/// [`crate::preset::RenderPresetConfig`].
#[derive(Debug, Clone, Copy)]
pub struct PathTraceConfig {
    pub width: u32,
    pub height: u32,
    pub samples_per_pixel: u32,
    pub max_bounces: u32,
    pub tile_size: u32,
    pub russian_roulette_min_bounces: u32,
    /// Threshold below which a tile is considered converged and stops
    /// sampling. Set to 0 to disable adaptive sampling.
    pub adaptive_threshold: f32,
    /// Camera projection model used when generating primary rays.
    pub projection: CameraProjection,
}

impl PathTraceConfig {
    pub fn preview() -> Self {
        Self {
            width: 640,
            height: 480,
            samples_per_pixel: 16,
            max_bounces: 4,
            tile_size: 64,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: CameraProjection::Perspective,
        }
    }

    pub fn final_quality() -> Self {
        Self {
            width: 1920,
            height: 1080,
            samples_per_pixel: 256,
            max_bounces: 8,
            tile_size: 64,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.01,
            projection: CameraProjection::Perspective,
        }
    }

    /// Default panorama configuration: 4096 × 2048 equirectangular at
    /// 512 samples per pixel. Aspect ratio is forced to 2:1 so the
    /// `[0, 2π]` longitude maps to one full screen-space rotation per
    /// row.
    pub fn panorama() -> Self {
        Self {
            width: 4096,
            height: 2048,
            samples_per_pixel: 512,
            max_bounces: 8,
            tile_size: 128,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.01,
            projection: CameraProjection::Equirectangular,
        }
    }
}

/// Tile in the accumulation buffer. Each pixel stores RGB radiance sums
/// plus the count of contributing samples for averaging.
#[derive(Debug, Clone)]
pub struct AccumulationBuffer {
    pub width: u32,
    pub height: u32,
    /// Length = width * height; each entry is `[r_sum, g_sum, b_sum, samples]`.
    pub pixels: Vec<[f32; 4]>,
}

impl AccumulationBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0.0; 4]; (width as usize) * (height as usize)],
        }
    }

    pub fn average_rgb(&self) -> Vec<[f32; 3]> {
        self.pixels
            .iter()
            .map(|p| {
                let n = p[3].max(1.0);
                [p[0] / n, p[1] / n, p[2] / n]
            })
            .collect()
    }

    /// Tone-map this buffer to an sRGB-8 byte triplet array without
    /// taking ownership. For large final renders (e.g. 1920×1080)
    /// this avoids the ~8 MB allocation that
    /// [`AccumulationBuffer::into_srgb8`] would force by consuming
    /// `self`. The tone-mapping pipeline (Reinhard + gamma 2.2) is
    /// kept identical so the two helpers are byte-for-byte
    /// equivalent.
    pub fn as_srgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 3);
        for p in &self.pixels {
            let n = p[3].max(1.0);
            let r = (p[0] / n).max(0.0);
            let g = (p[1] / n).max(0.0);
            let b = (p[2] / n).max(0.0);
            // Tone-map (Reinhard) then gamma 2.2.
            let r = (r / (1.0 + r)).powf(1.0 / 2.2);
            let g = (g / (1.0 + g)).powf(1.0 / 2.2);
            let b = (b / (1.0 + b)).powf(1.0 / 2.2);
            out.push((r * 255.0).round().clamp(0.0, 255.0) as u8);
            out.push((g * 255.0).round().clamp(0.0, 255.0) as u8);
            out.push((b * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        out
    }

    /// Owning variant of [`AccumulationBuffer::as_srgb8`]. Kept for
    /// callers that already consume the buffer at the encode site —
    /// internally it just forwards to `as_srgb8`.
    pub fn into_srgb8(self) -> Vec<u8> {
        self.as_srgb8()
    }
}

/// Progress callback type. Receives `(completed_tiles, total_tiles)`.
pub type ProgressFn = Arc<dyn Fn(u32, u32) + Send + Sync + 'static>;

/// Cooperative-cancellation handle shared with the render loop.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Render the scene into a fresh accumulation buffer.
pub fn render(
    scene: &PathTraceScene,
    camera: &RenderCamera,
    config: &PathTraceConfig,
    progress: Option<ProgressFn>,
    cancel: Option<CancelToken>,
) -> AccumulationBuffer {
    let mut accum = AccumulationBuffer::new(config.width, config.height);
    let tiles = generate_tiles(config.width, config.height, config.tile_size);
    let total_tiles = tiles.len() as u32;
    let completed = Arc::new(AtomicU32::new(0));

    // Each tile is rendered into a local accumulation chunk; we then
    // splat the chunk back into the global buffer in a serial pass.
    let results: Vec<(Tile, Vec<[f32; 4]>)> = tiles
        .par_iter()
        .map(|tile| {
            let cancel = cancel.as_ref();
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return (*tile, Vec::new());
            }
            let chunk = render_tile(scene, camera, config, *tile);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(cb) = progress.as_ref() {
                cb(done, total_tiles);
            }
            (*tile, chunk)
        })
        .collect();

    for (tile, chunk) in results {
        if chunk.is_empty() {
            continue;
        }
        let tw = tile.x_end - tile.x_start;
        for y in 0..(tile.y_end - tile.y_start) {
            for x in 0..tw {
                let gi = ((tile.y_start + y) * config.width + (tile.x_start + x)) as usize;
                let li = (y * tw + x) as usize;
                let p = &mut accum.pixels[gi];
                p[0] += chunk[li][0];
                p[1] += chunk[li][1];
                p[2] += chunk[li][2];
                p[3] += chunk[li][3];
            }
        }
    }
    accum
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    pub x_start: u32,
    pub y_start: u32,
    pub x_end: u32,
    pub y_end: u32,
}

impl Tile {
    /// Number of pixels in this tile.
    pub fn pixel_count(&self) -> usize {
        ((self.x_end - self.x_start) as usize) * ((self.y_end - self.y_start) as usize)
    }

    pub fn width(&self) -> u32 {
        self.x_end - self.x_start
    }

    pub fn height(&self) -> u32 {
        self.y_end - self.y_start
    }
}

/// Result of a single rendering pass over a tile, with per-pixel
/// sums *and* Welford sum-of-squared-deviations for variance tracking.
#[derive(Debug, Clone)]
pub struct TilePassResult {
    pub tile: Tile,
    /// One [r_sum, g_sum, b_sum, sample_count] per pixel, ordered row-major.
    pub sums: Vec<[f32; 4]>,
    /// Per-pixel sum of squared deviations from the running mean (Welford
    /// M2) for each RGB channel, used by the scheduler to estimate noise.
    pub sums_sq: Vec<[f32; 3]>,
}

/// Render `samples_this_pass` more samples into a tile, returning sums +
/// Welford-style sum-of-squared-deviations. Stateless — the caller is
/// responsible for accumulating into a buffer. Used by
/// [`crate::scheduler::TileScheduler`].
pub fn render_tile_pass(
    scene: &PathTraceScene,
    camera: &RenderCamera,
    config: &PathTraceConfig,
    tile: Tile,
    samples_this_pass: u32,
    rng_seed: u64,
) -> TilePassResult {
    let tw = tile.width() as usize;
    let th = tile.height() as usize;
    let pixel_count = tw * th;
    let mut sums = vec![[0.0_f32; 4]; pixel_count];
    let mut sums_sq = vec![[0.0_f32; 3]; pixel_count];
    let mut rng = fastrand::Rng::with_seed(rng_seed);

    let view = build_view(camera);
    let aspect = config.width as f32 / config.height.max(1) as f32;
    let half_h = focal_to_half_height(camera.focal_length_mm).max(1e-4);
    let half_w = half_h * aspect;

    for ly in 0..th {
        for lx in 0..tw {
            let li = ly * tw + lx;
            let mut mean = [0.0_f32; 3];
            let mut m2 = [0.0_f32; 3];
            for s in 0..samples_this_pass {
                let px = tile.x_start + lx as u32;
                let py = tile.y_start + ly as u32;
                let jx = rng.f32();
                let jy = rng.f32();
                let dir_world = match config.projection {
                    CameraProjection::Perspective => {
                        let nx = (px as f32 + jx) / config.width as f32 * 2.0 - 1.0;
                        let ny = 1.0 - (py as f32 + jy) / config.height as f32 * 2.0;
                        let dir_view = Vec3::new(nx * half_w, ny * half_h, -1.0).normalize();
                        view.basis * dir_view
                    }
                    CameraProjection::Equirectangular => equirectangular_dir(
                        px as f32 + jx,
                        py as f32 + jy,
                        config.width,
                        config.height,
                        &view,
                    ),
                };
                let ray = Ray::new(view.origin, dir_world);
                let r = trace_path(scene, ray, config, &mut rng);
                let rgb = [r.x, r.y, r.z];
                let n = (s + 1) as f32;
                for c in 0..3 {
                    let delta = rgb[c] - mean[c];
                    mean[c] += delta / n;
                    let delta2 = rgb[c] - mean[c];
                    m2[c] += delta * delta2;
                }
            }
            // Convert means back to sums for the caller's accumulator,
            // and forward the M2 as-is. The accumulator can either store
            // sums (legacy path) or run a higher-level Welford merge
            // (scheduler path).
            let n = samples_this_pass as f32;
            sums[li] = [mean[0] * n, mean[1] * n, mean[2] * n, n];
            sums_sq[li] = m2;
        }
    }
    TilePassResult {
        tile,
        sums,
        sums_sq,
    }
}

pub(crate) fn generate_tiles(width: u32, height: u32, tile_size: u32) -> Vec<Tile> {
    let ts = tile_size.max(1);
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < height {
        let y_end = (y + ts).min(height);
        let mut x = 0u32;
        while x < width {
            let x_end = (x + ts).min(width);
            out.push(Tile {
                x_start: x,
                y_start: y,
                x_end,
                y_end,
            });
            x = x_end;
        }
        y = y_end;
    }
    out
}

fn render_tile(
    scene: &PathTraceScene,
    camera: &RenderCamera,
    config: &PathTraceConfig,
    tile: Tile,
) -> Vec<[f32; 4]> {
    let tw = (tile.x_end - tile.x_start) as usize;
    let th = (tile.y_end - tile.y_start) as usize;
    let mut buf = vec![[0.0_f32; 4]; tw * th];
    let mut rng = fastrand::Rng::with_seed(
        u64::from(tile.x_start).wrapping_mul(0x9E37_79B1_7F4A_7C15)
            ^ u64::from(tile.y_start).wrapping_mul(0xBB67_AE85_84CA_A73B),
    );

    let view = build_view(camera);
    let aspect = config.width as f32 / config.height.max(1) as f32;
    let half_h = (focal_to_half_height(camera.focal_length_mm)).max(1e-4);
    let half_w = half_h * aspect;

    for ly in 0..th {
        for lx in 0..tw {
            let mut accum = Vec3::ZERO;
            for _ in 0..config.samples_per_pixel {
                let px = tile.x_start + lx as u32;
                let py = tile.y_start + ly as u32;
                let jx = rng.f32();
                let jy = rng.f32();
                let dir_world = match config.projection {
                    CameraProjection::Perspective => {
                        let nx = (px as f32 + jx) / config.width as f32 * 2.0 - 1.0;
                        let ny = 1.0 - (py as f32 + jy) / config.height as f32 * 2.0;
                        let dir_view = Vec3::new(nx * half_w, ny * half_h, -1.0).normalize();
                        view.basis * dir_view
                    }
                    CameraProjection::Equirectangular => equirectangular_dir(
                        px as f32 + jx,
                        py as f32 + jy,
                        config.width,
                        config.height,
                        &view,
                    ),
                };
                let ray = Ray::new(view.origin, dir_world);
                let radiance = trace_path(scene, ray, config, &mut rng);
                accum += radiance;
            }
            // Store `[r_sum, g_sum, b_sum, sample_count]` so the output
            // matches the documented `AccumulationBuffer` "sums + count"
            // convention. Pre-averaging (`avg, 1.0`) would silently produce
            // wrong sample counts if a caller ever additively merges tiles
            // (e.g. for progressive refinement); keep the invariant uniform
            // with `render_tile_pass` so the buffer is composable.
            let n = config.samples_per_pixel.max(1) as f32;
            let li = ly * tw + lx;
            buf[li] = [accum.x, accum.y, accum.z, n];
        }
    }
    buf
}

struct ViewFrame {
    origin: Vec3,
    basis: Mat3,
}

fn build_view(camera: &RenderCamera) -> ViewFrame {
    let origin = Vec3::from_array(camera.position_mm) * 0.001;
    let target = Vec3::from_array(camera.target_mm) * 0.001;
    let forward = (target - origin).normalize_or_zero();
    let forward = if forward.length_squared() < 1e-8 {
        Vec3::NEG_Z
    } else {
        forward
    };
    let world_up = Vec3::Y;
    let right = forward.cross(world_up).normalize_or_zero();
    let right = if right.length_squared() < 1e-8 {
        Vec3::X
    } else {
        right
    };
    let up = right.cross(forward).normalize();
    // Camera basis: columns are right, up, -forward.
    let basis = Mat3::from_cols(right, up, -forward);
    ViewFrame { origin, basis }
}

fn focal_to_half_height(focal_mm: f32) -> f32 {
    // Standard 35mm full-frame: sensor height 24mm.
    let sensor_h_mm: f32 = 24.0;
    let f = focal_mm.max(1.0);
    (sensor_h_mm * 0.5) / f
}

/// Generate a world-space direction for an equirectangular pixel.
///
/// The horizontal axis maps to longitude and the vertical axis maps to
/// latitude `[0, π]` (north pole at `y=0`, south pole at `y=height`).
/// We shift longitude by `-π` (`phi = (u - 0.5) * TAU`) so the panorama
/// centre column (`u = 0.5`) points along the camera-view `-Z`
/// (i.e. the camera forward), matching the standard 360°/VR convention
/// used by Insta360, Google PhotoSphere, FB 360, and every consumer VR
/// runtime. After the shift the four cardinal columns in view space
/// are:
///
/// | `u`     | `phi`   | view-space direction at the equator |
/// |---------|---------|-------------------------------------|
/// | `0.0`   | `-π`    | `+Z` (camera-backward)              |
/// | `0.25`  | `-π/2`  | `-X` (camera-left)                  |
/// | `0.5`   | `0`     | `-Z` (camera-forward)               |
/// | `0.75`  | `+π/2`  | `+X` (camera-right)                 |
/// | `1.0`   | `+π`    | `+Z` (camera-backward, wraps to 0)  |
///
/// The view-space direction is then multiplied by the camera basis so
/// the panorama is aimed via the camera's `target_mm`. The basis is
/// orthonormal, so `dir_view` is unit-length by construction
/// (`sin²θ (sin²φ + cos²φ) + cos²θ = 1`); we still call
/// `normalize_or_zero` on the result to absorb floating-point drift
/// from the basis multiplication and to give a deterministic value on
/// the degenerate `basis * dir_view == 0` case.
fn equirectangular_dir(px: f32, py: f32, width: u32, height: u32, view: &ViewFrame) -> Vec3 {
    let u = px / width.max(1) as f32;
    let v = py / height.max(1) as f32;
    // Centre-forward convention — see the table in the doc comment.
    let phi = (u - 0.5) * std::f32::consts::TAU;
    let theta = v * std::f32::consts::PI;
    let sin_theta = theta.sin();
    let dir_view = Vec3::new(sin_theta * phi.sin(), theta.cos(), -sin_theta * phi.cos());
    (view.basis * dir_view).normalize_or_zero()
}

/// Sum the radiance from analytic lights (sun, area) whose support
/// contains `ray.dir`. Used by `trace_path` to add direct visibility of
/// these lights for rays that escape the scene without hitting any
/// triangle — without this, looking straight at a sun would render
/// only the sky background.
///
/// Point/IES lights are intentionally excluded: they are point delta
/// emitters with zero solid angle, so a ray can never "hit" one.
fn direct_visible_lights(lights: &[NativeLight], ray: &Ray) -> Vec3 {
    let mut total = Vec3::ZERO;
    for light in lights {
        match light {
            NativeLight::Sun {
                direction,
                radiance,
                angular_radius_rad,
            } => {
                // The sun's apparent direction is `-direction` (the
                // direction light *comes from*). A primary ray points
                // away from the camera; it "hits" the sun if its
                // direction lies inside the sun's angular cone.
                let to_sun = -*direction;
                let cos_cone = angular_radius_rad.cos();
                let cos_angle = ray.dir.normalize_or_zero().dot(to_sun.normalize_or_zero());
                if cos_angle >= cos_cone {
                    total += *radiance;
                }
            }
            NativeLight::Area {
                position,
                normal,
                u_axis,
                v_axis,
                width,
                height,
                radiance,
            } => {
                // Ray-plane intersection: solve t such that the ray
                // crosses the plane defined by `position`/`normal`,
                // then check the (u, v) hit point is inside the
                // rectangle. The area light is two-sided so the dot
                // product sign is irrelevant.
                let denom = normal.dot(ray.dir);
                if denom.abs() < 1e-6 {
                    continue;
                }
                let t = (*position - ray.origin).dot(*normal) / denom;
                if t < ray.t_min || t > ray.t_max {
                    continue;
                }
                let hit_pt = ray.origin + ray.dir * t;
                let local = hit_pt - *position;
                let u = local.dot(*u_axis);
                let v = local.dot(*v_axis);
                if u.abs() <= *width * 0.5 && v.abs() <= *height * 0.5 {
                    total += *radiance;
                }
            }
            NativeLight::Point { .. } | NativeLight::Ies { .. } => {
                // Delta luminaires have zero solid angle; a ray cannot
                // intersect them in the geometric sense, so they
                // contribute nothing to direct-miss visibility. Their
                // illumination flows entirely through NEE.
            }
        }
    }
    total
}

fn trace_path(
    scene: &PathTraceScene,
    ray_in: Ray,
    config: &PathTraceConfig,
    rng: &mut fastrand::Rng,
) -> Vec3 {
    let mut radiance = Vec3::ZERO;
    let mut throughput = Vec3::ONE;
    let mut ray = ray_in;
    let mut last_was_specular = true;
    let mut prev_bsdf_pdf = 1.0_f32;

    for bounce in 0..config.max_bounces {
        let hit = closest_hit(&scene.bvh, &scene.triangles, &ray);
        let Some(hit) = hit else {
            let env = environment_radiance(&scene.sky, ray.dir);
            radiance += throughput * env;
            // Cycles parity: a ray that escapes the scene also sees any
            // analytic light whose support contains `ray.dir`. Without
            // this branch a panorama or a "shoot ray at the sky"
            // primary ray would never observe the sun disk, only the
            // diffuse sky background. Sun lights are evaluated as a
            // delta-cone test; area lights as a ray-rectangle test.
            // Direct-lighting NEE already accounts for these analytic
            // lights inside the bouncing loop, so we skip the analytic
            // contribution on rays that came from a BSDF sample to
            // avoid double-counting (`last_was_specular` is true for
            // the primary ray and for rays after specular bounces).
            if last_was_specular {
                radiance += throughput * direct_visible_lights(&scene.lights, &ray);
            }
            break;
        };
        let tri = &scene.triangles[hit.prim_id as usize];
        let shading = &scene.shading_data[hit.prim_id as usize];
        let mat = scene.material_for(hit.prim_id);

        // Smooth-normal interpolation; fall back to geometric if degenerate.
        let w = 1.0 - hit.u - hit.v;
        let smooth = shading.n0 * w + shading.n1 * hit.u + shading.n2 * hit.v;
        let n_shade = if smooth.length_squared() < 1e-8 {
            geom_normal(tri)
        } else {
            smooth.normalize()
        };
        let n = if n_shade.dot(-ray.dir) > 0.0 {
            n_shade
        } else {
            -n_shade
        };

        let hit_pos = ray.at(hit.t);
        let wo = -ray.dir;

        // Self-emission: only contribute on bounce 0 OR if we just took
        // a specular bounce (no NEE was attempted).
        if last_was_specular {
            radiance += throughput * mat.emissive;
        }

        // Next-event estimation for each light.
        for light in &scene.lights {
            if let Some(ls) = sample_light(light, hit_pos, [rng.f32(), rng.f32()]) {
                let cos_at_surface = n.dot(ls.direction);
                if cos_at_surface <= 0.0 || ls.pdf <= 0.0 {
                    continue;
                }
                let mut shadow = Ray::new(hit_pos, ls.direction);
                shadow.t_max = (ls.distance - 1e-3).max(1e-3);
                if any_hit(&scene.bvh, &scene.triangles, &shadow) {
                    continue;
                }
                let f = eval_bsdf(&mat, n, ls.direction, wo);
                let bsdf_pdf = pdf_bsdf(&mat, n, ls.direction, wo);
                let mis = if is_delta(light) {
                    1.0
                } else {
                    power_heuristic(ls.pdf, bsdf_pdf)
                };
                let contribution = throughput * f * ls.emitted * cos_at_surface * mis / ls.pdf;
                radiance += contribution;
            }
        }

        // Sample the BSDF for the next bounce.
        let r = [rng.f32(), rng.f32(), rng.f32()];
        let Some(sample) = sample_bsdf(&mat, n, wo, r) else {
            break;
        };
        throughput *= sample.weight;
        prev_bsdf_pdf = sample.pdf;
        last_was_specular = sample.is_specular;

        // Russian roulette after the configured warm-up bounces.
        if bounce >= config.russian_roulette_min_bounces {
            let p_continue = throughput.max_element().clamp(0.05, 0.95);
            if rng.f32() > p_continue {
                break;
            }
            throughput /= p_continue;
        }

        ray = Ray::new(hit_pos, sample.direction);
    }

    // Silence unused warning when last_was_specular ends specular but no
    // further bounce happens — we still want the prev_bsdf_pdf available
    // for callers that may extend this loop with environment MIS.
    let _ = prev_bsdf_pdf;

    radiance
}

fn matrix_from_array(m: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    m
}

fn transform_point(m: &[[f32; 4]; 4], p: Vec3) -> Vec3 {
    let x = m[0][0] * p.x + m[0][1] * p.y + m[0][2] * p.z + m[0][3];
    let y = m[1][0] * p.x + m[1][1] * p.y + m[1][2] * p.z + m[1][3];
    let z = m[2][0] * p.x + m[2][1] * p.y + m[2][2] * p.z + m[2][3];
    let w = m[3][0] * p.x + m[3][1] * p.y + m[3][2] * p.z + m[3][3];
    if w.abs() < 1e-12 {
        Vec3::new(x, y, z)
    } else {
        Vec3::new(x / w, y / w, z / w)
    }
}

fn normal_matrix(m: &[[f32; 4]; 4]) -> Mat3 {
    let upper = Mat3::from_cols_array_2d(&[
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]);
    // Inverse transpose for non-uniform scale safety; for orthonormal
    // rotations this is the same matrix.
    upper.inverse().transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{RenderLight, SerializedMesh};

    fn identity_matrix() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn make_quad_mesh(material_id: Option<&str>) -> SerializedMesh {
        SerializedMesh {
            id: "q".into(),
            indices: vec![0, 1, 2, 0, 2, 3],
            positions: vec![
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
            normals: vec![
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            material_id: material_id.map(str::to_string),
            transform: identity_matrix(),
        }
    }

    fn cornell_box_scene() -> RenderScene {
        // Minimal closed Cornell-like room: floor quad + back wall + a
        // top quad acting as an area light position.
        let mut scene = RenderScene::new();
        let floor = SerializedMesh {
            id: "floor".into(),
            indices: vec![0, 1, 2, 0, 2, 3],
            positions: vec![
                [-2.0, 0.0, -2.0],
                [2.0, 0.0, -2.0],
                [2.0, 0.0, 2.0],
                [-2.0, 0.0, 2.0],
            ],
            normals: vec![
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            uvs: vec![[0.0, 0.0]; 4],
            material_id: Some("floor".into()),
            transform: identity_matrix(),
        };
        scene.push_mesh(floor);
        scene.push_light(RenderLight::Point {
            position_mm: [0.0, 2000.0, 0.0],
            intensity: 50.0,
            color_temperature_k: 5500.0,
        });
        scene
    }

    #[test]
    fn accumulation_buffer_averages_correctly() {
        let mut buf = AccumulationBuffer::new(2, 2);
        buf.pixels[0] = [1.0, 2.0, 3.0, 1.0];
        buf.pixels[1] = [2.0, 2.0, 2.0, 2.0];
        let avg = buf.average_rgb();
        assert_eq!(avg[0], [1.0, 2.0, 3.0]);
        assert_eq!(avg[1], [1.0, 1.0, 1.0]);
    }

    #[test]
    fn tile_generator_covers_full_image() {
        let tiles = generate_tiles(100, 80, 32);
        let mut area = 0u32;
        for t in &tiles {
            area += (t.x_end - t.x_start) * (t.y_end - t.y_start);
        }
        assert_eq!(area, 100 * 80);
    }

    #[test]
    fn tile_generator_respects_image_bounds() {
        let tiles = generate_tiles(50, 50, 32);
        for t in &tiles {
            assert!(t.x_end <= 50);
            assert!(t.y_end <= 50);
            assert!(t.x_start < t.x_end);
            assert!(t.y_start < t.y_end);
        }
    }

    #[test]
    fn empty_scene_returns_environment_radiance() {
        let scene = RenderScene::new();
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 1500.0, 5000.0],
            target_mm: [0.0, 1500.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let sky = SkyParams {
            strength: 1.0,
            color: [0.5, 0.5, 0.5],
            turbidity: 2.0,
        };
        let mats: Vec<PathTraceMaterial> = vec![];
        let pt = PathTraceScene::from_render_scene(&scene, mats, |_| None, sky);
        let cfg = PathTraceConfig {
            width: 8,
            height: 8,
            samples_per_pixel: 4,
            max_bounces: 2,
            tile_size: 8,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = render(&pt, &camera, &cfg, None, None);
        // Every pixel should equal the environment radiance (0.5, 0.5, 0.5).
        let avg = buf.average_rgb();
        for px in &avg {
            assert!((px[0] - 0.5).abs() < 1e-3);
            assert!((px[1] - 0.5).abs() < 1e-3);
            assert!((px[2] - 0.5).abs() < 1e-3);
        }
    }

    #[test]
    fn cornell_floor_lit_by_point_light_is_positive() {
        let scene = cornell_box_scene();
        let mut materials = vec![PathTraceMaterial::default_grey()];
        materials[0].base_color = Vec3::new(0.8, 0.8, 0.8);
        let pt = PathTraceScene::from_render_scene(
            &scene,
            materials,
            |id| if id == "floor" { Some(0) } else { None },
            SkyParams {
                strength: 0.0,
                color: [0.0; 3],
                turbidity: 2.0,
            },
        );
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 1500.0, 4000.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let cfg = PathTraceConfig {
            width: 16,
            height: 16,
            samples_per_pixel: 4,
            max_bounces: 2,
            tile_size: 16,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = render(&pt, &camera, &cfg, None, None);
        let avg = buf.average_rgb();
        // Floor should have at least one bright pixel from the point light.
        let max_lum = avg
            .iter()
            .map(|p| p[0] + p[1] + p[2])
            .fold(0.0_f32, f32::max);
        assert!(max_lum > 0.0, "expected some lit pixels, got max {max_lum}");
    }

    #[test]
    fn progress_callback_fires_for_every_tile() {
        let scene = RenderScene::new();
        let pt = PathTraceScene::from_render_scene(
            &scene,
            vec![],
            |_| None,
            SkyParams {
                strength: 1.0,
                color: [0.1; 3],
                turbidity: 2.0,
            },
        );
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let counter = Arc::new(AtomicU32::new(0));
        let cb_counter = counter.clone();
        let progress: ProgressFn = Arc::new(move |done, _| {
            cb_counter.store(done, Ordering::Relaxed);
        });
        let cfg = PathTraceConfig {
            width: 16,
            height: 16,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        render(&pt, &camera, &cfg, Some(progress), None);
        assert_eq!(counter.load(Ordering::Relaxed), 4); // 2x2 tiles
    }

    #[test]
    fn cancellation_short_circuits_render() {
        let scene = RenderScene::new();
        let pt = PathTraceScene::from_render_scene(
            &scene,
            vec![],
            |_| None,
            SkyParams {
                strength: 1.0,
                color: [0.1; 3],
                turbidity: 2.0,
            },
        );
        let camera = RenderCamera {
            id: "c".into(),
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 4.0,
        };
        let token = CancelToken::new();
        token.cancel();
        let cfg = PathTraceConfig {
            width: 16,
            height: 16,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 3,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = render(&pt, &camera, &cfg, None, Some(token));
        // Cancelled before any tile was rendered → all pixels untouched.
        for p in &buf.pixels {
            assert_eq!(p, &[0.0, 0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn material_lookup_fallback_to_default_grey() {
        let scene = RenderScene::new();
        let pt = PathTraceScene::from_render_scene(
            &scene,
            vec![],
            |_| None,
            SkyParams {
                strength: 1.0,
                color: [0.5; 3],
                turbidity: 2.0,
            },
        );
        let m = pt.material_for(99);
        assert_eq!(m.base_color, Vec3::splat(0.6));
    }

    #[test]
    fn scene_builds_with_mesh_transform_applied() {
        let mut scene = RenderScene::new();
        let mut m = make_quad_mesh(None);
        // Translate the quad to (10, 0, 0).
        m.transform = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        scene.push_mesh(m);
        let pt = PathTraceScene::from_render_scene(
            &scene,
            vec![],
            |_| None,
            SkyParams {
                strength: 1.0,
                color: [0.5; 3],
                turbidity: 2.0,
            },
        );
        let b = pt.bvh.root_bounds();
        assert!(b.min.x > 8.0 && b.max.x < 12.0);
    }
}
