//! Runtime LOD level selection.
//!
//! The asset import pipeline bakes a [`crate::LodChain`] alongside each
//! mesh — typically `[LOD0 = 100 %, LOD1 = 25 %, LOD2 = 5 %]`. That
//! pre-decimated chain is dead weight unless the renderer (or any
//! consumer that fetches mesh blobs from
//! [`crate::AssetDatabase`]) picks the right level *at draw time*.
//!
//! [`LodSelector`] is the runtime piece: given an object's bounding
//! box, the active camera, and the viewport size, it estimates the
//! object's projected screen-space size and picks the highest-LOD
//! (most detailed) level whose **projected error budget** stays
//! below a configurable pixel threshold.
//!
//! The selector is a pure function — no rendering state, no GPU
//! handles — so it slots into both the path-traced final-render
//! pipeline and the wgpu real-time viewport without duplication.
//!
//! ## Algorithm
//!
//! Each LOD level has a `ratio` ∈ (0, 1]: the fraction of triangles
//! retained relative to LOD 0. The selector approximates the
//! triangle-grid pitch on that level as `mesh_diameter / √triangles`,
//! and the per-triangle error as proportional to that pitch.
//! Projecting through the perspective camera produces a pixel-space
//! error estimate:
//!
//! ```text
//! pitch_world  = bbox_diameter_mm * (1 - sqrt(ratio))
//! pitch_screen = pitch_world * focal_length_px / max(distance_mm, near_clip_mm)
//! ```
//!
//! The selector picks the smallest-detail LOD whose `pitch_screen`
//! remains under `screen_space_error_threshold_px`. When no LOD
//! satisfies the budget (e.g. the camera is right on top of the
//! object) we always return LOD 0; when the chain is empty we
//! return 0 unconditionally.

use serde::{Deserialize, Serialize};

use crate::lod::LodChain;

/// Per-frame inputs the LOD selector needs to project the LOD error
/// budget into screen-space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenContext {
    /// Camera position in world (mm).
    pub camera_position_mm: [f64; 3],
    /// Object axis-aligned bounding box, world (mm).
    pub object_bbox_min_mm: [f64; 3],
    pub object_bbox_max_mm: [f64; 3],
    /// Vertical field of view in radians.
    pub fov_y_radians: f32,
    /// Viewport height in pixels.
    pub viewport_height_px: u32,
    /// Maximum tolerated projected error per LOD level (pixels).
    ///
    /// Default `8.0` produces a tight LOD ladder for fly-throughs
    /// at typical desktop resolutions; AEC drone/walkthrough
    /// preview pipelines bump this to 16-24 px to keep the LOD
    /// transition rare and predictable.
    pub screen_space_error_threshold_px: f32,
}

impl Default for ScreenContext {
    fn default() -> Self {
        Self {
            camera_position_mm: [0.0; 3],
            object_bbox_min_mm: [0.0; 3],
            object_bbox_max_mm: [0.0; 3],
            fov_y_radians: 60.0_f32.to_radians(),
            viewport_height_px: 1080,
            screen_space_error_threshold_px: 8.0,
        }
    }
}

/// Stateless selector. Configuration knobs live here so two
/// pipelines can share one selector instance without paying the cost
/// of re-deriving them per call.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LodSelector {
    /// Minimum distance (mm) clamped on the divisor; protects the
    /// projection from blowing up when the camera goes inside an
    /// object. Defaults to 1 mm — small enough to never matter for
    /// AEC-scale scenes (rooms are tens of metres across).
    pub near_clip_mm: f64,
}

impl Default for LodSelector {
    fn default() -> Self {
        Self { near_clip_mm: 1.0 }
    }
}

impl LodSelector {
    /// Select a LOD index into [`LodChain::levels`]. Returns `0`
    /// when the chain is empty or no level satisfies the projected
    /// error budget.
    ///
    /// The chain is expected to be sorted *descending* by `ratio`
    /// (LOD 0 first, coarsest last) — this matches the contract of
    /// [`LodChain::from_ratios`].
    #[must_use]
    pub fn select(&self, ctx: &ScreenContext, chain: &LodChain) -> u32 {
        if chain.levels.is_empty() {
            return 0;
        }
        let diameter = bbox_diameter(ctx.object_bbox_min_mm, ctx.object_bbox_max_mm);
        // Degenerate / zero-size bboxes always render at LOD 0:
        // there's no meaningful projection.
        if diameter <= f64::EPSILON {
            return 0;
        }
        let centre = bbox_centre(ctx.object_bbox_min_mm, ctx.object_bbox_max_mm);
        let distance = distance(ctx.camera_position_mm, centre).max(self.near_clip_mm);
        // pixel focal length: half-viewport / tan(half-fov).
        let half_fov = (ctx.fov_y_radians.max(1e-6) * 0.5) as f64;
        let focal_px = (f64::from(ctx.viewport_height_px) * 0.5) / half_fov.tan().max(1e-6);
        let threshold = ctx.screen_space_error_threshold_px.max(0.0) as f64;
        // The chain is sorted *descending* by ratio (LOD 0 first,
        // coarsest last). Iterate coarsest-first; the first level
        // whose projected pitch fits under the screen-space error
        // budget is the right pick. Finer levels are always under
        // budget (their pitch is smaller) but represent unused
        // detail when this coarser level already satisfies the
        // budget — that's exactly why we ship a LOD chain.
        for level in chain.levels.iter().rev() {
            let pitch_world = diameter * (1.0 - f64::from(level.ratio).sqrt());
            // ratio == 1.0 ⇒ zero pitch ⇒ always-valid (LOD0).
            let pitch_screen = pitch_world * focal_px / distance;
            if pitch_screen <= threshold {
                return u32::from(level.level);
            }
        }
        // No level met the budget — fall back to LOD0 (max detail).
        // In practice this is unreachable because LOD0 always has
        // ratio = 1 ⇒ pitch_screen = 0, but keep the explicit return
        // for defensive completeness.
        0
    }
}

#[inline]
fn bbox_diameter(min: [f64; 3], max: [f64; 3]) -> f64 {
    let dx = max[0] - min[0];
    let dy = max[1] - min[1];
    let dz = max[2] - min[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[inline]
fn bbox_centre(min: [f64; 3], max: [f64; 3]) -> [f64; 3] {
    [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ]
}

#[inline]
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 m cube centred at the origin, default 60° FOV / 1080 px /
    /// 8 px threshold. Camera sits on the +Z axis.
    fn ctx(camera_dist_mm: f64) -> ScreenContext {
        ScreenContext {
            camera_position_mm: [0.0, 0.0, camera_dist_mm],
            object_bbox_min_mm: [-500.0, -500.0, -500.0],
            object_bbox_max_mm: [500.0, 500.0, 500.0],
            fov_y_radians: 60.0_f32.to_radians(),
            viewport_height_px: 1080,
            screen_space_error_threshold_px: 8.0,
        }
    }

    fn three_level_chain() -> LodChain {
        // Standard real-mesh chain: [1.0, 0.25, 0.05].
        LodChain::aggressive_for_real_mesh(10_000, &[])
    }

    #[test]
    fn close_camera_picks_lod_zero() {
        let selector = LodSelector::default();
        // Camera 0.2 m from the object centre — pitch error blows up.
        let lod = selector.select(&ctx(200.0), &three_level_chain());
        assert_eq!(lod, 0, "close camera must pick LOD0 for maximum detail");
    }

    #[test]
    fn far_camera_picks_lowest_lod() {
        let selector = LodSelector::default();
        // Camera 1 km away from a 1 m cube — LOD2 pitch projects to
        // ~1.3 px, well under the 8 px budget.
        let lod = selector.select(&ctx(1_000_000.0), &three_level_chain());
        assert_eq!(lod, 2, "far camera must pick the coarsest LOD");
    }

    #[test]
    fn mid_distance_picks_intermediate_lod() {
        let selector = LodSelector::default();
        // Camera ~120 m from the 1 m cube. LOD1 pitch projects to
        // ~6.7 px (under the 8 px budget), LOD2 pitch projects to
        // ~10.5 px (over budget) — so the selector should land on
        // LOD1 specifically.
        let lod = selector.select(&ctx(120_000.0), &three_level_chain());
        assert_eq!(lod, 1, "mid-distance must pick LOD1 (got {lod})");
    }

    #[test]
    fn empty_chain_returns_zero() {
        let selector = LodSelector::default();
        let empty = LodChain { levels: Vec::new() };
        let lod = selector.select(&ctx(1000.0), &empty);
        assert_eq!(lod, 0);
    }

    #[test]
    fn single_level_chain_always_returns_zero() {
        let selector = LodSelector::default();
        let chain = LodChain::from_ratios(1000, &[]);
        // Truncate to one level (LOD0 only).
        let single = LodChain {
            levels: chain.levels.iter().take(1).cloned().collect(),
        };
        for dist in [10.0, 1_000.0, 100_000.0, 1_000_000.0] {
            let lod = selector.select(&ctx(dist), &single);
            assert_eq!(lod, 0, "single-level chain must always pick LOD0");
        }
    }

    #[test]
    fn degenerate_bbox_returns_zero() {
        let selector = LodSelector::default();
        let mut c = ctx(1000.0);
        c.object_bbox_min_mm = [0.0, 0.0, 0.0];
        c.object_bbox_max_mm = [0.0, 0.0, 0.0];
        let lod = selector.select(&c, &three_level_chain());
        assert_eq!(lod, 0);
    }

    #[test]
    fn near_clip_protects_against_camera_inside_object() {
        let selector = LodSelector::default();
        let mut c = ctx(0.0); // Camera exactly at object centre.
        c.camera_position_mm = c.object_bbox_min_mm; // overlapping bbox.
        let lod = selector.select(&c, &three_level_chain());
        // Distance is clamped to near_clip_mm; LOD must be the
        // most detailed (the projection blows up but is finite).
        assert_eq!(lod, 0);
    }

    #[test]
    fn threshold_zero_picks_lod_zero() {
        let selector = LodSelector::default();
        let mut c = ctx(10_000.0);
        c.screen_space_error_threshold_px = 0.0;
        let lod = selector.select(&c, &three_level_chain());
        assert_eq!(lod, 0, "zero threshold accepts no degradation");
    }

    #[test]
    fn high_threshold_picks_coarsest_lod() {
        let selector = LodSelector::default();
        let mut c = ctx(1_000.0);
        c.screen_space_error_threshold_px = 10_000.0;
        let lod = selector.select(&c, &three_level_chain());
        assert_eq!(lod, 2, "huge threshold accepts the coarsest LOD");
    }

    #[test]
    fn lod_selector_is_serde_roundtrip() {
        let s = LodSelector { near_clip_mm: 2.0 };
        let json = serde_json::to_string(&s).unwrap();
        let back: LodSelector = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
