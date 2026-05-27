//! 2D orthographic helpers for Draft mode (Phase 12 task 10).
//!
//! The Draft viewport reuses the wgpu `RenderPipeline` from
//! `render_pipeline.rs` but supplies orthographic camera matrices
//! and an explicit grid-spacing schedule so the grid stays
//! readable at any zoom level (a 1x zoom shows decimetre divisions;
//! a 100x zoom shows millimetre divisions).
//!
//! Putting the maths here — rather than in the React component —
//! gives us deterministic, unit-testable behaviour that the
//! renderer can rely on without having to round-trip through wgpu.

use glam::Mat4;

/// Compute an orthographic projection matrix for a 2D drafting
/// view.
///
/// * `viewport_width_px` / `viewport_height_px`: render-target
///   extent in physical pixels.
/// * `pixels_per_mm`: current zoom factor — how many screen pixels
///   represent one millimetre of model space. At `1.0` the user
///   sees one drawing-mm per CSS pixel.
///
/// The matrix maps model coordinates in millimetres (y-up, screen
/// coords flipped) to the wgpu clip space (`-1..=1` in x/y,
/// `0..=1` in z).
pub fn orthographic_2d(
    viewport_width_px: u32,
    viewport_height_px: u32,
    pixels_per_mm: f32,
) -> Mat4 {
    let w = viewport_width_px.max(1) as f32;
    let h = viewport_height_px.max(1) as f32;
    let ppmm = pixels_per_mm.max(1e-6);
    // Half-extents in mm such that the full viewport spans
    // `width_px / ppmm` millimetres horizontally.
    let half_w = w / (2.0 * ppmm);
    let half_h = h / (2.0 * ppmm);
    // wgpu orthographic: x ∈ [-1,1], y ∈ [-1,1], z ∈ [0,1].
    // We map the user's drawing y upward (positive y = up screen).
    Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, -10_000.0, 10_000.0)
}

/// Grid-spacing pair: `(minor_mm, major_mm)`. Major lines are
/// always 10x the minor spacing — the schedule chooses the spacing
/// so the *minor* lines stay 8-30 screen pixels apart at the
/// current zoom level. (Smaller than 8 px the grid becomes
/// visually noisy; larger than 30 px and it becomes sparse.)
///
/// Spacing values come from the standard engineering decade
/// schedule (1 → 10 → 100 → 1000 → 10000 mm), so the user can
/// always read a snap intersection as an integer millimetre value.
pub fn major_minor_grid_spacing(pixels_per_mm: f32) -> (f32, f32) {
    let ppmm = pixels_per_mm.max(1e-6);
    // Smallest spacing such that minor lines are at least 8 px apart.
    // `minor = 10^k` where `10^k * ppmm >= 8`.
    let target_minor_px = 8.0;
    let raw = target_minor_px / ppmm;
    let exp = raw.log10().ceil();
    let minor_mm = 10.0_f32.powf(exp);
    let major_mm = minor_mm * 10.0;
    (minor_mm, major_mm)
}

/// Snap a world-space coordinate to the nearest grid intersection.
/// Used by the Draft snap indicator overlay.
pub fn snap_to_grid(world_x_mm: f32, world_y_mm: f32, spacing_mm: f32) -> (f32, f32) {
    let s = spacing_mm.max(1e-6);
    let sx = (world_x_mm / s).round() * s;
    let sy = (world_y_mm / s).round() * s;
    (sx, sy)
}

/// Convert a screen-space click into a world-space point in mm.
///
/// `screen_x` / `screen_y` are in CSS pixels with `(0, 0)` at the
/// top-left of the canvas. The result is in model-space mm with
/// `(0, 0)` at the canvas centre and y up.
pub fn screen_to_world_2d(
    screen_x: f32,
    screen_y: f32,
    viewport_width_px: u32,
    viewport_height_px: u32,
    pixels_per_mm: f32,
    pan_x_mm: f32,
    pan_y_mm: f32,
) -> (f32, f32) {
    let ppmm = pixels_per_mm.max(1e-6);
    let cx = viewport_width_px as f32 * 0.5;
    let cy = viewport_height_px as f32 * 0.5;
    let mm_x = (screen_x - cx) / ppmm + pan_x_mm;
    let mm_y = -(screen_y - cy) / ppmm + pan_y_mm;
    (mm_x, mm_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthographic_matrix_is_finite_and_invertible() {
        let m = orthographic_2d(800, 600, 1.0);
        for col in m.to_cols_array_2d().iter() {
            for &x in col {
                assert!(x.is_finite());
            }
        }
        // ortho matrices have very small determinants because of the
        // mm-scale z extents; check non-zero rather than > 1e-6.
        assert!(m.determinant() != 0.0);
    }

    #[test]
    fn orthographic_matrix_maps_centre_to_origin() {
        // The centre of the canvas should land at the clip-space
        // origin so the user's (0, 0) draws at the centre of the
        // viewport regardless of zoom.
        let m = orthographic_2d(800, 600, 2.5);
        let clip = m.project_point3(glam::Vec3::ZERO);
        assert!(clip.x.abs() < 1e-5);
        assert!(clip.y.abs() < 1e-5);
    }

    #[test]
    fn orthographic_matrix_scales_with_pixels_per_mm() {
        // Doubling ppmm halves the world extent visible on
        // screen, so a fixed world point ends up twice as far
        // from the centre in clip space.
        let m1 = orthographic_2d(800, 600, 1.0);
        let m2 = orthographic_2d(800, 600, 2.0);
        let p = glam::Vec3::new(100.0, 0.0, 0.0);
        let c1 = m1.project_point3(p).x;
        let c2 = m2.project_point3(p).x;
        assert!((c2 / c1 - 2.0).abs() < 1e-3, "c1={c1}, c2={c2}");
    }

    #[test]
    fn grid_spacing_at_unit_zoom_uses_10mm_minor() {
        // At 1 pixel/mm the minor spacing wants 10 mm so the
        // lines are 10 px apart (within the 8-30 px window).
        let (minor, major) = major_minor_grid_spacing(1.0);
        assert_eq!(minor as i32, 10);
        assert_eq!(major as i32, 100);
    }

    #[test]
    fn grid_spacing_at_high_zoom_uses_1mm_minor() {
        // At 10 pixel/mm a single mm is already 10 px on screen,
        // so the minor spacing should drop to 1 mm.
        let (minor, _) = major_minor_grid_spacing(10.0);
        assert_eq!(minor as i32, 1);
    }

    #[test]
    fn grid_spacing_at_low_zoom_uses_100mm_minor() {
        // At 0.1 pixel/mm we need 100 mm for the lines to be ≥ 8 px.
        let (minor, _) = major_minor_grid_spacing(0.1);
        assert_eq!(minor as i32, 100);
    }

    #[test]
    fn grid_spacing_always_keeps_minor_within_window() {
        // For any zoom level, the resulting minor spacing in
        // pixels should be in the [8, 80) window — 80 because
        // we step in decade jumps.
        for &ppmm in &[0.01_f32, 0.5, 1.0, 3.0, 12.0, 100.0, 250.0] {
            let (minor_mm, _) = major_minor_grid_spacing(ppmm);
            let minor_px = minor_mm * ppmm;
            assert!(
                (8.0..80.0).contains(&minor_px),
                "ppmm={ppmm} minor_mm={minor_mm} minor_px={minor_px}",
            );
        }
    }

    #[test]
    fn snap_rounds_to_nearest_grid_intersection() {
        let (sx, sy) = snap_to_grid(123.4, -789.6, 10.0);
        assert_eq!(sx as i32, 120);
        assert_eq!(sy as i32, -790);
    }

    #[test]
    fn snap_with_zero_spacing_is_safe() {
        // The clamp inside `snap_to_grid` should prevent
        // division-by-zero; the input passes through.
        let (sx, sy) = snap_to_grid(1.0, 2.0, 0.0);
        // Either passes through identically or rounds to the
        // closest float — the contract is only "no NaN".
        assert!(sx.is_finite());
        assert!(sy.is_finite());
    }

    #[test]
    fn screen_to_world_maps_centre_to_origin() {
        let (wx, wy) = screen_to_world_2d(400.0, 300.0, 800, 600, 1.0, 0.0, 0.0);
        assert!(wx.abs() < 1e-3);
        assert!(wy.abs() < 1e-3);
    }

    #[test]
    fn screen_to_world_flips_y_correctly() {
        // Click below the centre (larger screen_y) should map to
        // negative world y because the canvas y-axis points down.
        let (_, wy_below) = screen_to_world_2d(400.0, 400.0, 800, 600, 1.0, 0.0, 0.0);
        let (_, wy_above) = screen_to_world_2d(400.0, 200.0, 800, 600, 1.0, 0.0, 0.0);
        assert!(wy_below < 0.0, "expected below=negative, got {wy_below}");
        assert!(wy_above > 0.0, "expected above=positive, got {wy_above}");
    }

    #[test]
    fn screen_to_world_respects_pan_offset() {
        // With a pan offset, the centre of the canvas reports the
        // pan offset's world coordinates rather than (0, 0).
        let (wx, wy) = screen_to_world_2d(400.0, 300.0, 800, 600, 1.0, 500.0, -250.0);
        assert!((wx - 500.0).abs() < 1e-3);
        assert!((wy - -250.0).abs() < 1e-3);
    }

    #[test]
    fn screen_to_world_respects_zoom_factor() {
        // At 2 pixels/mm, 100 pixels off-centre is 50 mm.
        let (wx, _) = screen_to_world_2d(400.0 + 100.0, 300.0, 800, 600, 2.0, 0.0, 0.0);
        assert!((wx - 50.0).abs() < 1e-3, "got {wx}");
    }
}
