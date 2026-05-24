// Single-letter math identifiers (n, m, p) are kept to match the Welford
// online-variance literature; expanding them obscures the algebra.
#![allow(clippy::many_single_char_names)]

//! Tile scheduler with progressive sampling, adaptive convergence
//! detection, and cancellation support.
//!
//! Native Rust replacement for Cycles' [`RenderScheduler`]
//! (`src/integrator/render_scheduler.h` / `render_scheduler.cpp`). The
//! scheduler drives the CPU path tracer in [`crate::path_trace`] in
//! multi-pass mode:
//!
//! * The image is partitioned into tiles ([`crate::path_trace::Tile`]).
//! * Each *pass* takes a small batch of samples per pixel for every
//!   non-converged tile (Rayon-parallel across tiles).
//! * Per-pixel running statistics are kept (`Welford` mean + sum of
//!   squared deviations) so the scheduler can stop sampling tiles whose
//!   relative noise has dropped below `adaptive_threshold`.
//! * Cancellation is cooperative via [`crate::path_trace::CancelToken`]
//!   and checked between passes and between tiles.
//!
//! The scheduler does NOT own the rendering kernel — it composes the
//! kernel exposed by [`crate::path_trace::render_tile_pass`]. This keeps
//! the kernel testable in isolation and lets the GPU backend
//! ([`crate::gpu_trace`]) plug in via the same interface in PR2 follow-ups.

use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::path_trace::{
    generate_tiles, render_tile_pass, render_tile_pass_with_aux, AccumulationBuffer, CancelToken,
    PathTraceConfig, PathTraceScene, ProgressFn, Tile,
};
use crate::scene::RenderCamera;

/// Configuration for the tile scheduler.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerConfig {
    /// Total samples-per-pixel budget the scheduler may spend before
    /// stopping (whether or not the tile is converged).
    pub max_samples_per_pixel: u32,
    /// Samples per pass per pixel. Smaller passes give finer-grained
    /// adaptive cut-off and better cancellation responsiveness; larger
    /// passes amortise the BVH/material lookups.
    pub samples_per_pass: u32,
    /// Tile size in pixels. Picked from [`crate::preset::RenderPresetConfig::tile_size_px`]
    /// upstream.
    pub tile_size: u32,
    /// Relative-noise threshold below which a tile is considered
    /// converged. Computed as `mean(per-pixel-variance) / mean(per-pixel-mean²)`
    /// (luminance-only), so it is unit-less and tone-mapping-invariant.
    /// Disable adaptive sampling by setting this to `0.0` or a negative
    /// value.
    pub adaptive_threshold: f32,
    /// Minimum samples-per-pixel a tile must have before we trust the
    /// variance estimate enough to early-out on.
    pub min_samples_before_check: u32,
    /// When `true`, the scheduler drives the kernel via
    /// [`render_tile_pass_with_aux`] and splats the per-pass first-hit
    /// albedo / normal / depth sums into the [`AccumulationBuffer`]'s
    /// aux channels (which is initialised via
    /// [`AccumulationBuffer::new_with_aux`]).
    ///
    /// This is the *production* path for feature-guided denoising: the
    /// downstream [`crate::final_render::encode_srgb8`] consults the
    /// buffer's aux channels and feeds them to the bilateral kernel.
    /// Without aux capture, the scheduler produced an aux-less buffer
    /// even when the preset asked for denoising, and the bilateral
    /// kernel silently fell back to luminance-only filtering — i.e.
    /// PR-J's render-fidelity improvement was invisible in production.
    ///
    /// Cost: aux capture adds three `Vec<[f32; 3 | 1]>` per pass plus a
    /// per-pixel splat into the buffer's aux channels. In practice
    /// ~3-5% of the per-pass cost on a `samples_per_pass = 16` budget.
    pub capture_aux: bool,
}

impl SchedulerConfig {
    /// Reasonable defaults for an interactive preview.
    pub fn preview() -> Self {
        Self {
            max_samples_per_pixel: 32,
            samples_per_pass: 4,
            tile_size: 64,
            adaptive_threshold: 0.01,
            min_samples_before_check: 8,
            // Preview path is denoise-off-by-default and latency-sensitive;
            // skip aux. Upgrade to `true` if a preview preset switches on
            // denoise.
            capture_aux: false,
        }
    }

    /// Reasonable defaults for a final render — note this is paired with
    /// a [`PathTraceConfig`] whose `samples_per_pixel` matches
    /// `samples_per_pass`; the scheduler controls the total via
    /// `max_samples_per_pixel`.
    pub fn final_quality() -> Self {
        Self {
            max_samples_per_pixel: 1024,
            samples_per_pass: 16,
            tile_size: 64,
            adaptive_threshold: 0.005,
            min_samples_before_check: 32,
            // Final-quality presets always denoise; aux guidance is the
            // whole point of PR-J's bilateral upgrade.
            capture_aux: true,
        }
    }

    /// Disable adaptive sampling — always run `max_samples_per_pixel`.
    pub fn fixed_sample_count(samples: u32, tile_size: u32) -> Self {
        Self {
            max_samples_per_pixel: samples,
            samples_per_pass: samples.clamp(1, 16),
            tile_size,
            adaptive_threshold: -1.0,
            min_samples_before_check: u32::MAX,
            // Fixed-count is typically a benchmark / reference render;
            // leave aux off so the output matches the no-denoise baseline
            // by default. Callers wanting aux-guided denoising should
            // build the config explicitly with `capture_aux: true`.
            capture_aux: false,
        }
    }
}

/// Per-tile state the scheduler tracks between passes.
#[derive(Debug, Clone)]
struct TileState {
    tile: Tile,
    /// Per-pixel Welford running mean (RGB).
    mean: Vec<[f32; 3]>,
    /// Per-pixel Welford M2 (sum of squared deviations).
    m2: Vec<[f32; 3]>,
    samples: u32,
    converged: bool,
}

impl TileState {
    fn new(tile: Tile) -> Self {
        let n = tile.pixel_count();
        Self {
            tile,
            mean: vec![[0.0; 3]; n],
            m2: vec![[0.0; 3]; n],
            samples: 0,
            converged: false,
        }
    }

    /// Merge a pass result into the running per-pixel statistics using
    /// the parallel-batch Welford update (Chan, Golub & LeVeque 1979).
    fn merge_pass(&mut self, sums: &[[f32; 4]], sums_sq: &[[f32; 3]], pass_samples: u32) {
        if pass_samples == 0 || sums.is_empty() {
            return;
        }
        let n_a = self.samples;
        let n_b = pass_samples;
        let n = n_a + n_b;
        let nf = n as f32;
        let n_a_f = n_a as f32;
        let n_b_f = n_b as f32;

        for i in 0..self.mean.len() {
            let pass_mean = [sums[i][0] / n_b_f, sums[i][1] / n_b_f, sums[i][2] / n_b_f];
            for c in 0..3 {
                let delta = pass_mean[c] - self.mean[i][c];
                let new_mean = (n_a_f * self.mean[i][c] + n_b_f * pass_mean[c]) / nf;
                self.m2[i][c] += sums_sq[i][c] + delta * delta * n_a_f * n_b_f / nf;
                self.mean[i][c] = new_mean;
            }
        }
        self.samples = n;
    }

    /// Per-tile relative noise: average per-pixel variance / average
    /// per-pixel mean² + small epsilon. Uses luminance (Rec. 709) so the
    /// metric is tone-mapping-invariant.
    fn relative_noise(&self) -> f32 {
        if self.samples < 2 {
            return f32::INFINITY;
        }
        let denom = (self.samples - 1) as f32;
        let mut var_sum = 0.0f32;
        let mut mean_sum_sq = 0.0f32;
        for i in 0..self.mean.len() {
            let m = self.mean[i];
            let v = [
                self.m2[i][0] / denom,
                self.m2[i][1] / denom,
                self.m2[i][2] / denom,
            ];
            // Rec. 709 luminance.
            let lum_var = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
            let lum_mean = 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
            var_sum += lum_var;
            mean_sum_sq += lum_mean * lum_mean;
        }
        let pixels = self.mean.len() as f32;
        let avg_var = var_sum / pixels;
        let avg_mean_sq = mean_sum_sq / pixels;
        avg_var / (avg_mean_sq + 1.0e-6)
    }
}

/// Status reported from each pass for orchestration / UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchedulerProgress {
    pub pass: u32,
    pub samples_per_pixel: u32,
    pub tiles_total: u32,
    pub tiles_converged: u32,
    pub cancelled: bool,
}

/// Final outcome of a scheduled render.
#[derive(Debug, Clone)]
pub struct SchedulerOutcome {
    pub buffer: AccumulationBuffer,
    pub tiles_total: u32,
    pub tiles_converged: u32,
    pub passes_run: u32,
    pub samples_per_pixel: u32,
    pub cancelled: bool,
}

/// Optional progress callback for the scheduler.
pub type SchedulerProgressFn = Arc<dyn Fn(SchedulerProgress) + Send + Sync + 'static>;

/// Top-level scheduler entry point. Pure CPU.
pub fn schedule(
    scene: &PathTraceScene,
    camera: &RenderCamera,
    base_config: &PathTraceConfig,
    sched: &SchedulerConfig,
    cancel: Option<CancelToken>,
    progress: Option<SchedulerProgressFn>,
    inner_progress: Option<ProgressFn>,
) -> SchedulerOutcome {
    let _ = inner_progress; // reserved for per-tile UI hookup (Tasks 8+9)
                            // Allocate the aux channels up-front when capture is requested.
                            // Doing this once outside the pass loop avoids checking on every
                            // pass and keeps the down-stream `buffer.has_aux()` invariant
                            // monotone over the render's lifetime.
    let mut buffer = if sched.capture_aux {
        AccumulationBuffer::new_with_aux(base_config.width, base_config.height)
    } else {
        AccumulationBuffer::new(base_config.width, base_config.height)
    };
    let tiles = generate_tiles(base_config.width, base_config.height, sched.tile_size);
    let mut tile_states: Vec<TileState> = tiles.iter().copied().map(TileState::new).collect();
    let tiles_total = tile_states.len() as u32;

    let max_pass_samples = sched.samples_per_pass.max(1);
    let max_total = sched.max_samples_per_pixel.max(max_pass_samples);
    let mut samples_done = 0u32;
    let mut pass = 0u32;
    let mut cancelled = false;

    while samples_done < max_total {
        if let Some(c) = cancel.as_ref() {
            if c.is_cancelled() {
                cancelled = true;
                break;
            }
        }
        let samples_remaining = max_total - samples_done;
        let pass_samples = samples_remaining.min(max_pass_samples);

        // Run the next pass for every non-converged tile in parallel.
        let pending: Vec<usize> = tile_states
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (!t.converged).then_some(i))
            .collect();
        if pending.is_empty() {
            break;
        }

        let pass_seed_base: u64 = u64::from(pass).wrapping_mul(0xA24B_AED4_963E_E407);

        let results: Vec<(usize, Option<crate::path_trace::TilePassResult>)> = pending
            .par_iter()
            .map(|&idx| {
                let cancel_ref = cancel.as_ref();
                if cancel_ref.is_some_and(CancelToken::is_cancelled) {
                    return (idx, None);
                }
                let state = &tile_states[idx];
                let tile = state.tile;
                let samples_so_far = state.samples;
                let seed = pass_seed_base
                    ^ u64::from(tile.x_start).wrapping_mul(0x9E37_79B1_7F4A_7C15)
                    ^ u64::from(tile.y_start).wrapping_mul(0xBB67_AE85_84CA_A73B);
                // Pass `samples_so_far` so the Halton index advances
                // contiguously across passes — see render_tile_pass docs.
                // Adaptive convergence may stop sampling some tiles
                // early, so per-tile sample counters drift from the
                // global `samples_done` (which is the same for all
                // tiles in a pass). The per-tile `state.samples` is
                // therefore the load-bearing value here, not the
                // global `samples_done`.
                //
                // When `capture_aux` is on we route through the aux
                // variant so the [`TilePassResult`] carries first-hit
                // albedo / normal / depth sums that the scheduler can
                // splat into the buffer's aux channels below.
                let result = if sched.capture_aux {
                    render_tile_pass_with_aux(
                        scene,
                        camera,
                        base_config,
                        tile,
                        pass_samples,
                        samples_so_far,
                        seed,
                    )
                } else {
                    render_tile_pass(
                        scene,
                        camera,
                        base_config,
                        tile,
                        pass_samples,
                        samples_so_far,
                        seed,
                    )
                };
                (idx, Some(result))
            })
            .collect();

        for (idx, result) in results {
            let Some(result) = result else {
                continue;
            };
            // Splat sums into the global accumulation buffer (for
            // downstream tonemap / display). When aux capture is
            // enabled and the kernel emitted aux sums, splat those
            // into the buffer's aux channels too so
            // [`crate::final_render::encode_srgb8`] can feed the
            // bilateral kernel real first-hit guidance.
            let tile = tile_states[idx].tile;
            let tw = tile.width();
            for ly in 0..tile.height() {
                for lx in 0..tw {
                    let li = (ly * tw + lx) as usize;
                    let gi =
                        ((tile.y_start + ly) * base_config.width + (tile.x_start + lx)) as usize;
                    let p = &mut buffer.pixels[gi];
                    p[0] += result.sums[li][0];
                    p[1] += result.sums[li][1];
                    p[2] += result.sums[li][2];
                    p[3] += result.sums[li][3];
                }
            }
            // Aux splat — gated on both sides actually having aux
            // (defensive: a future kernel-routing tweak that produced
            // a `TilePassResult` without aux against an aux-allocated
            // buffer would silently drop the splat rather than panic).
            if let (Some(buf_albedo), Some(pass_albedo)) =
                (buffer.albedo.as_mut(), result.albedo_sums.as_ref())
            {
                for ly in 0..tile.height() {
                    for lx in 0..tw {
                        let li = (ly * tw + lx) as usize;
                        let gi = ((tile.y_start + ly) * base_config.width + (tile.x_start + lx))
                            as usize;
                        for c in 0..3 {
                            buf_albedo[gi][c] += pass_albedo[li][c];
                        }
                    }
                }
            }
            if let (Some(buf_normal), Some(pass_normal)) =
                (buffer.normal.as_mut(), result.normal_sums.as_ref())
            {
                for ly in 0..tile.height() {
                    for lx in 0..tw {
                        let li = (ly * tw + lx) as usize;
                        let gi = ((tile.y_start + ly) * base_config.width + (tile.x_start + lx))
                            as usize;
                        for c in 0..3 {
                            buf_normal[gi][c] += pass_normal[li][c];
                        }
                    }
                }
            }
            if let (Some(buf_depth), Some(pass_depth)) =
                (buffer.depth.as_mut(), result.depth_sums.as_ref())
            {
                for ly in 0..tile.height() {
                    for lx in 0..tw {
                        let li = (ly * tw + lx) as usize;
                        let gi = ((tile.y_start + ly) * base_config.width + (tile.x_start + lx))
                            as usize;
                        buf_depth[gi] += pass_depth[li];
                    }
                }
            }
            // Update Welford running stats.
            tile_states[idx].merge_pass(&result.sums, &result.sums_sq, pass_samples);
        }

        samples_done += pass_samples;
        pass += 1;

        // Adaptive convergence check (skip if disabled or below the
        // minimum-sample bar).
        if sched.adaptive_threshold > 0.0 {
            for state in tile_states.iter_mut() {
                if state.converged || state.samples < sched.min_samples_before_check {
                    continue;
                }
                if state.relative_noise() < sched.adaptive_threshold {
                    state.converged = true;
                }
            }
        }

        let converged = tile_states.iter().filter(|t| t.converged).count() as u32;
        if let Some(cb) = progress.as_ref() {
            cb(SchedulerProgress {
                pass,
                samples_per_pixel: samples_done,
                tiles_total,
                tiles_converged: converged,
                cancelled,
            });
        }

        // Early-out when every tile has converged.
        if converged == tiles_total {
            break;
        }
    }

    let tiles_converged = tile_states.iter().filter(|t| t.converged).count() as u32;
    SchedulerOutcome {
        buffer,
        tiles_total,
        tiles_converged,
        passes_run: pass,
        samples_per_pixel: samples_done,
        cancelled,
    }
}

/// Helper used by the queue / job system: convert a render preset into a
/// scheduler config that respects the user's adaptive/quality knobs.
pub fn config_from_preset(preset: &crate::preset::RenderPresetConfig) -> SchedulerConfig {
    let mut samples_per_pass = match preset.samples {
        s if s <= 16 => s.max(1),
        s if s <= 64 => 8,
        s if s <= 256 => 16,
        _ => 32,
    };
    samples_per_pass = samples_per_pass.max(1);
    SchedulerConfig {
        max_samples_per_pixel: preset.samples.max(1),
        samples_per_pass,
        tile_size: preset.tile_size_px.max(8),
        // Tight default — interior scenes need it. Set to 0 to disable
        // adaptive sampling from the UI.
        adaptive_threshold: 0.005,
        min_samples_before_check: samples_per_pass * 2,
        // Aux capture follows the preset's `denoise` flag. Aux guidance
        // is only useful when the downstream tone-mapper actually runs
        // the bilateral kernel, so a preset with `denoise = false` has
        // no reason to pay for aux accumulation, and a preset with
        // `denoise = true` should always have it. This keeps the
        // "scheduler produces an aux-less buffer for a denoise=true
        // preset" anti-pattern flagged by Devin Review out of
        // production.
        capture_aux: preset.denoise,
    }
}

/// Statistics observer hook (Mutex-wrapped so callers can poll it from
/// the UI thread without recompiling the kernel). Optional convenience
/// for the queue / bridge layer.
#[derive(Debug, Default)]
pub struct SchedulerStats {
    pub passes_run: u32,
    pub samples_per_pixel: u32,
    pub tiles_total: u32,
    pub tiles_converged: u32,
    pub cancelled: bool,
}

/// Shared observer adaptor — wraps a [`SchedulerStats`] in `Arc<Mutex<_>>`
/// and produces a [`SchedulerProgressFn`] that copies the latest
/// snapshot into it.
pub fn make_progress_observer() -> (Arc<Mutex<SchedulerStats>>, SchedulerProgressFn) {
    let stats = Arc::new(Mutex::new(SchedulerStats::default()));
    let stats_for_cb = stats.clone();
    let cb: SchedulerProgressFn = Arc::new(move |p: SchedulerProgress| {
        if let Ok(mut s) = stats_for_cb.lock() {
            s.passes_run = p.pass;
            s.samples_per_pixel = p.samples_per_pixel;
            s.tiles_total = p.tiles_total;
            s.tiles_converged = p.tiles_converged;
            s.cancelled = p.cancelled;
        }
    });
    (stats, cb)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::lighting::SkyParams;
    use crate::path_trace::PathTraceConfig;
    use crate::scene::{RenderLight, RenderScene, SerializedMesh};

    fn identity_matrix() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn floor_mesh() -> SerializedMesh {
        SerializedMesh {
            id: "floor".into(),
            positions: vec![
                [-1.0, 0.0, -1.0],
                [1.0, 0.0, -1.0],
                [1.0, 0.0, 1.0],
                [-1.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            indices: vec![0, 1, 2, 0, 2, 3],
            material_id: None,
            uvs: vec![[0.0, 0.0]; 4],
            transform: identity_matrix(),
        }
    }

    fn scene_with_floor_and_light() -> PathTraceScene {
        let mut scene = RenderScene::default();
        scene.push_mesh(floor_mesh());
        scene.push_light(RenderLight::Point {
            position_mm: [0.0, 1000.0, 0.0],
            intensity: 1500.0,
            color_temperature_k: 5500.0,
        });
        PathTraceScene::from_render_scene(&scene, vec![], |_| None, SkyParams::default())
    }

    fn small_config() -> PathTraceConfig {
        PathTraceConfig {
            width: 32,
            height: 24,
            samples_per_pixel: 1, // ignored by scheduler; pass-size controls
            max_bounces: 2,
            tile_size: 8,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        }
    }

    fn small_camera() -> RenderCamera {
        RenderCamera {
            id: "cam".into(),
            position_mm: [0.0, 1500.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        }
    }

    #[test]
    fn fixed_sample_scheduler_covers_all_tiles_with_requested_samples() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        let sched = SchedulerConfig::fixed_sample_count(8, cfg.tile_size);
        let outcome = schedule(&scene, &camera, &cfg, &sched, None, None, None);
        assert_eq!(outcome.tiles_total, 12, "32x24 with 8px tiles = 4*3 = 12");
        assert!(outcome.samples_per_pixel >= 8);
        let expected_tiles = 12;
        assert!(outcome.passes_run >= 1);
        // Every pixel in every tile should have received the same number
        // of samples — fixed-sample-count must be deterministic.
        let n = cfg.width as usize * cfg.height as usize;
        for i in 0..n {
            let s = outcome.buffer.pixels[i][3];
            assert!(
                (s - 8.0).abs() < 1e-3,
                "pixel {i}: expected 8 samples, got {s}"
            );
        }
        let _ = expected_tiles;
    }

    #[test]
    fn adaptive_scheduler_stops_when_tile_converges() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        // A *very* loose threshold so the scheduler converges fast.
        let sched = SchedulerConfig {
            max_samples_per_pixel: 64,
            samples_per_pass: 4,
            tile_size: cfg.tile_size,
            adaptive_threshold: 1.0e6, // effectively any noise level converges
            min_samples_before_check: 4,
            capture_aux: false,
        };
        let outcome = schedule(&scene, &camera, &cfg, &sched, None, None, None);
        assert!(outcome.passes_run <= 4, "{:?}", outcome);
        assert_eq!(outcome.tiles_converged, outcome.tiles_total);
        assert!(outcome.samples_per_pixel < 64);
    }

    #[test]
    fn cancellation_short_circuits_scheduler() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        let sched = SchedulerConfig {
            max_samples_per_pixel: 1024,
            samples_per_pass: 16,
            tile_size: cfg.tile_size,
            adaptive_threshold: 0.0,
            min_samples_before_check: 64,
            capture_aux: false,
        };
        let token = CancelToken::new();
        token.cancel();
        let outcome = schedule(&scene, &camera, &cfg, &sched, Some(token), None, None);
        assert!(outcome.cancelled);
        assert_eq!(outcome.passes_run, 0);
        assert_eq!(outcome.samples_per_pixel, 0);
    }

    #[test]
    fn progress_callback_fires_each_pass() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        let sched = SchedulerConfig::fixed_sample_count(12, cfg.tile_size);
        let calls = Arc::new(AtomicU32::new(0));
        let calls_for_cb = calls.clone();
        let cb: SchedulerProgressFn = Arc::new(move |_p: SchedulerProgress| {
            calls_for_cb.fetch_add(1, Ordering::SeqCst);
        });
        let outcome = schedule(&scene, &camera, &cfg, &sched, None, Some(cb), None);
        let observed = calls.load(Ordering::SeqCst);
        assert!(observed >= outcome.passes_run);
    }

    #[test]
    fn welford_merge_preserves_sample_count() {
        let mut state = TileState::new(Tile {
            x_start: 0,
            y_start: 0,
            x_end: 4,
            y_end: 4,
        });
        let sums = vec![[2.0, 2.0, 2.0, 4.0]; 16];
        let sums_sq = vec![[0.5, 0.5, 0.5]; 16];
        state.merge_pass(&sums, &sums_sq, 4);
        assert_eq!(state.samples, 4);
        // Mean of an all-equal batch should be `sums / pass_samples`.
        for c in 0..3 {
            assert!((state.mean[0][c] - 0.5).abs() < 1e-6);
        }
        state.merge_pass(&sums, &sums_sq, 4);
        assert_eq!(state.samples, 8);
    }

    #[test]
    fn config_from_preset_picks_reasonable_passes() {
        use crate::preset::{RenderPresetConfig, RenderQuality};
        let preset = RenderPresetConfig {
            quality: RenderQuality::High,
            samples: 256,
            denoise: true,
            tile_size_px: 128,
            resolution_x: 1920,
            resolution_y: 1080,
            use_motion_blur: false,
            use_volumetric_atmosphere: false,
        };
        let sched = config_from_preset(&preset);
        assert_eq!(sched.max_samples_per_pixel, 256);
        assert_eq!(sched.tile_size, 128);
        assert!(sched.samples_per_pass <= 32);
        assert!(sched.samples_per_pass >= 1);
        // A preset with `denoise = true` MUST request aux capture from
        // the scheduler; otherwise the bilateral kernel silently falls
        // back to luminance-only filtering and PR-J's render-fidelity
        // improvement is invisible in production. The inverse holds
        // for `denoise = false` presets.
        assert!(sched.capture_aux);
    }

    #[test]
    fn config_from_preset_skips_aux_when_denoise_is_off() {
        use crate::preset::{RenderPresetConfig, RenderQuality};
        let preset = RenderPresetConfig {
            quality: RenderQuality::RealtimePreview,
            samples: 32,
            denoise: false,
            tile_size_px: 128,
            resolution_x: 1280,
            resolution_y: 720,
            use_motion_blur: false,
            use_volumetric_atmosphere: false,
        };
        let sched = config_from_preset(&preset);
        assert!(
            !sched.capture_aux,
            "denoise = false must not pay for aux accumulation"
        );
    }

    /// Round-trip: a scheduler with `capture_aux = true` must produce a
    /// buffer whose `albedo` / `normal` / `depth` channels are populated
    /// and non-trivially varying. This is the regression test for the
    /// design gap flagged by Devin Review on PR-J round-2 \u2014 prior to
    /// threading aux through the scheduler, every render driven through
    /// [`schedule`] produced an aux-less buffer, defeating the entire
    /// point of the aux-guided bilateral feature.
    #[test]
    fn scheduler_with_capture_aux_populates_buffer_aux_channels() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        let sched = SchedulerConfig {
            max_samples_per_pixel: 8,
            samples_per_pass: 4,
            tile_size: cfg.tile_size,
            adaptive_threshold: -1.0,
            min_samples_before_check: u32::MAX,
            capture_aux: true,
        };
        let outcome = schedule(&scene, &camera, &cfg, &sched, None, None, None);
        assert!(
            outcome.buffer.has_aux(),
            "capture_aux = true must produce a buffer with all aux channels"
        );

        // Averaged albedo must contain at least one pixel whose
        // luminance is non-zero (otherwise the kernel got an
        // entirely-black albedo and aux guidance has no signal).
        let albedo = outcome
            .buffer
            .average_albedo()
            .expect("aux buffer should yield albedo");
        let any_lit = albedo.iter().any(|p| (p[0] + p[1] + p[2]) > 1.0e-3);
        assert!(
            any_lit,
            "albedo channel must record at least one non-black first-hit"
        );

        // Averaged normal must contain at least one non-zero unit
        // vector (the average_normal() helper re-normalises; pixels
        // with no hits collapse to `[0,0,0]`).
        let normal = outcome
            .buffer
            .average_normal()
            .expect("aux buffer should yield normal");
        let any_normal = normal
            .iter()
            .any(|n| (n[0].abs() + n[1].abs() + n[2].abs()) > 0.5);
        assert!(
            any_normal,
            "normal channel must record at least one valid unit normal"
        );

        // Depth must contain at least one finite, positive depth (the
        // sky-miss sentinel is `1.0e6`, so a positive value below
        // that threshold proves a real surface hit was recorded).
        let depth = outcome
            .buffer
            .average_depth()
            .expect("aux buffer should yield depth");
        let any_surface = depth.iter().any(|&d| d > 0.0 && d < 1.0e5);
        assert!(
            any_surface,
            "depth channel must record at least one real surface hit"
        );
    }

    /// Inverse contract \u2014 a scheduler with `capture_aux = false` must
    /// NOT pay the aux accumulation cost. We assert the buffer has no
    /// aux channels at all so the bilateral path correctly degrades to
    /// luminance-only filtering (and the kernel's `None` branch is
    /// exercised).
    #[test]
    fn scheduler_without_capture_aux_produces_no_aux_buffer() {
        let scene = scene_with_floor_and_light();
        let camera = small_camera();
        let cfg = small_config();
        let sched = SchedulerConfig {
            max_samples_per_pixel: 8,
            samples_per_pass: 4,
            tile_size: cfg.tile_size,
            adaptive_threshold: -1.0,
            min_samples_before_check: u32::MAX,
            capture_aux: false,
        };
        let outcome = schedule(&scene, &camera, &cfg, &sched, None, None, None);
        assert!(
            !outcome.buffer.has_aux(),
            "capture_aux = false must not allocate aux channels"
        );
        assert!(outcome.buffer.albedo.is_none());
        assert!(outcome.buffer.normal.is_none());
        assert!(outcome.buffer.depth.is_none());
    }

    #[test]
    fn progress_observer_records_latest_snapshot() {
        let (stats, cb) = make_progress_observer();
        cb(SchedulerProgress {
            pass: 3,
            samples_per_pixel: 12,
            tiles_total: 8,
            tiles_converged: 5,
            cancelled: false,
        });
        let snapshot = stats.lock().unwrap();
        assert_eq!(snapshot.passes_run, 3);
        assert_eq!(snapshot.samples_per_pixel, 12);
        assert_eq!(snapshot.tiles_total, 8);
        assert_eq!(snapshot.tiles_converged, 5);
        assert!(!snapshot.cancelled);
    }
}
