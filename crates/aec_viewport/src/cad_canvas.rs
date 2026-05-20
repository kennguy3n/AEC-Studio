//! Native 2D CAD canvas state.
//!
//! Pure-Rust state + math for the Draft-mode 2D canvas. The wgpu surface
//! is owned by `renderer.rs`; this module owns the orthographic camera
//! (with infinite pan/zoom), grid, crosshair, rubber-band selection
//! rect, and snap-indicator overlay. All math is unit-testable without
//! a GPU.

use serde::{Deserialize, Serialize};

use crate::snap_overlay::{SnapHit, SnapKind};

/// World-space rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldRect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl WorldRect {
    pub fn from_two_points(a: [f64; 2], b: [f64; 2]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1])],
            max: [a[0].max(b[0]), a[1].max(b[1])],
        }
    }

    pub fn width(&self) -> f64 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f64 {
        self.max[1] - self.min[1]
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }

    pub fn intersects(&self, other: &WorldRect) -> bool {
        !(self.max[0] < other.min[0]
            || self.min[0] > other.max[0]
            || self.max[1] < other.min[1]
            || self.min[1] > other.max[1])
    }
}

/// 2D orthographic camera over world XY.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrthoCamera2D {
    /// World-space centre point.
    pub center: [f64; 2],
    /// World units per screen pixel. Smaller = more zoomed in.
    pub units_per_pixel: f64,
    /// Viewport dimensions in screen pixels.
    pub viewport_px: [u32; 2],
}

impl OrthoCamera2D {
    pub fn new(viewport_px: [u32; 2]) -> Self {
        Self {
            center: [0.0, 0.0],
            units_per_pixel: 1.0,
            viewport_px,
        }
    }

    pub fn viewport_world_size(&self) -> [f64; 2] {
        [
            self.viewport_px[0] as f64 * self.units_per_pixel,
            self.viewport_px[1] as f64 * self.units_per_pixel,
        ]
    }

    pub fn visible_world_rect(&self) -> WorldRect {
        let [w, h] = self.viewport_world_size();
        WorldRect {
            min: [self.center[0] - w / 2.0, self.center[1] - h / 2.0],
            max: [self.center[0] + w / 2.0, self.center[1] + h / 2.0],
        }
    }

    /// Pan by a screen-pixel delta (positive dx = pan right ⇒ centre
    /// moves left in world space).
    pub fn pan_pixels(&mut self, dx_px: f64, dy_px: f64) {
        self.center[0] -= dx_px * self.units_per_pixel;
        self.center[1] += dy_px * self.units_per_pixel;
    }

    /// Pan by a world-space delta.
    pub fn pan_world(&mut self, dx: f64, dy: f64) {
        self.center[0] += dx;
        self.center[1] += dy;
    }

    /// Zoom factor multiplies `units_per_pixel`. `factor < 1` zooms in,
    /// `> 1` zooms out. `cursor_px` (if Some) is the screen-pixel anchor
    /// the zoom is applied around (so cursor world coords stay fixed).
    pub fn zoom_about(&mut self, factor: f64, cursor_px: Option<[f64; 2]>) {
        let factor = factor.clamp(1e-6, 1e6);
        let cursor_px = cursor_px.unwrap_or([
            self.viewport_px[0] as f64 / 2.0,
            self.viewport_px[1] as f64 / 2.0,
        ]);
        let world_before = self.screen_to_world(cursor_px);
        self.units_per_pixel = (self.units_per_pixel * factor).clamp(1e-6, 1e9);
        let world_after = self.screen_to_world(cursor_px);
        self.center[0] += world_before[0] - world_after[0];
        self.center[1] += world_before[1] - world_after[1];
    }

    /// Screen pixel (origin top-left, +y down) → world (origin centre,
    /// +y up).
    pub fn screen_to_world(&self, p_px: [f64; 2]) -> [f64; 2] {
        let half = [
            self.viewport_px[0] as f64 / 2.0,
            self.viewport_px[1] as f64 / 2.0,
        ];
        [
            self.center[0] + (p_px[0] - half[0]) * self.units_per_pixel,
            self.center[1] - (p_px[1] - half[1]) * self.units_per_pixel,
        ]
    }

    pub fn world_to_screen(&self, p_world: [f64; 2]) -> [f64; 2] {
        let half = [
            self.viewport_px[0] as f64 / 2.0,
            self.viewport_px[1] as f64 / 2.0,
        ];
        [
            half[0] + (p_world[0] - self.center[0]) / self.units_per_pixel,
            half[1] - (p_world[1] - self.center[1]) / self.units_per_pixel,
        ]
    }

    /// Fit the viewport to the given world rectangle plus a padding
    /// factor (e.g. 1.1 = 10 % margin).
    pub fn fit_to_rect(&mut self, rect: WorldRect, padding: f64) {
        let pad = padding.max(1.0);
        let w = (rect.width().abs() * pad).max(1e-6);
        let h = (rect.height().abs() * pad).max(1e-6);
        let upp_x = w / self.viewport_px[0].max(1) as f64;
        let upp_y = h / self.viewport_px[1].max(1) as f64;
        self.units_per_pixel = upp_x.max(upp_y);
        self.center = [
            (rect.min[0] + rect.max[0]) * 0.5,
            (rect.min[1] + rect.max[1]) * 0.5,
        ];
    }
}

/// Grid configuration. Major grid lines every `major_spacing` world
/// units; minor every `major_spacing / subdivisions` units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CadGrid {
    pub major_spacing: f64,
    pub subdivisions: u32,
    pub visible: bool,
}

impl Default for CadGrid {
    fn default() -> Self {
        Self {
            major_spacing: 1000.0,
            subdivisions: 10,
            visible: true,
        }
    }
}

/// Generated grid line set, in world coordinates.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GridLines {
    pub minor_vertical: Vec<f64>,
    pub minor_horizontal: Vec<f64>,
    pub major_vertical: Vec<f64>,
    pub major_horizontal: Vec<f64>,
}

impl GridLines {
    pub fn total(&self) -> usize {
        self.minor_vertical.len()
            + self.minor_horizontal.len()
            + self.major_vertical.len()
            + self.major_horizontal.len()
    }
}

/// Compute grid line positions visible in `rect`. The grid spacing
/// auto-scales — at very high zoom-out we drop the minor grid and use
/// 10 × major spacing to avoid overdraw.
pub fn compute_grid(rect: &WorldRect, grid: &CadGrid, camera: &OrthoCamera2D) -> GridLines {
    if !grid.visible {
        return GridLines::default();
    }
    // Adaptive scale: choose the smallest power-of-10 multiplier of
    // major_spacing such that at least 4 px separates grid lines.
    let min_px_separation = 4.0;
    let min_world_separation = min_px_separation * camera.units_per_pixel;
    let mut effective_major = grid.major_spacing.max(f64::EPSILON);
    while effective_major < min_world_separation {
        effective_major *= 10.0;
    }
    let subdivisions = grid.subdivisions.max(1);
    let effective_minor = effective_major / subdivisions as f64;
    // Cap counts to protect against pathological zoom-outs.
    const CAP: usize = 10_000;

    let mut out = GridLines::default();
    // Major lines.
    let start_x = (rect.min[0] / effective_major).floor() * effective_major;
    let end_x = (rect.max[0] / effective_major).ceil() * effective_major;
    let mut x = start_x;
    while x <= end_x && out.major_vertical.len() < CAP {
        out.major_vertical.push(x);
        x += effective_major;
    }
    let start_y = (rect.min[1] / effective_major).floor() * effective_major;
    let end_y = (rect.max[1] / effective_major).ceil() * effective_major;
    let mut y = start_y;
    while y <= end_y && out.major_horizontal.len() < CAP {
        out.major_horizontal.push(y);
        y += effective_major;
    }
    // Minor lines — only if they'd be visible.
    if effective_minor * camera.units_per_pixel.recip() >= min_px_separation && subdivisions > 1 {
        let mut x = start_x;
        while x <= end_x && out.minor_vertical.len() < CAP {
            for k in 1..subdivisions {
                let v = x + effective_minor * k as f64;
                if v >= rect.min[0] && v <= rect.max[0] {
                    out.minor_vertical.push(v);
                }
            }
            x += effective_major;
        }
        let mut y = start_y;
        while y <= end_y && out.minor_horizontal.len() < CAP {
            for k in 1..subdivisions {
                let v = y + effective_minor * k as f64;
                if v >= rect.min[1] && v <= rect.max[1] {
                    out.minor_horizontal.push(v);
                }
            }
            y += effective_major;
        }
    }
    out
}

/// Crosshair cursor — two world-space lines spanning the visible rect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CrosshairLines {
    pub horizontal: [[f64; 2]; 2],
    pub vertical: [[f64; 2]; 2],
}

pub fn compute_crosshair(world_cursor: [f64; 2], rect: &WorldRect) -> CrosshairLines {
    CrosshairLines {
        horizontal: [
            [rect.min[0], world_cursor[1]],
            [rect.max[0], world_cursor[1]],
        ],
        vertical: [
            [world_cursor[0], rect.min[1]],
            [world_cursor[0], rect.max[1]],
        ],
    }
}

/// Rubber-band selection rectangle (screen-space).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RubberBand {
    pub start_world: [f64; 2],
    pub current_world: [f64; 2],
    /// True if the box was started "right-to-left" (crossing window).
    pub crossing: bool,
}

impl RubberBand {
    pub fn new(start_world: [f64; 2]) -> Self {
        Self {
            start_world,
            current_world: start_world,
            crossing: false,
        }
    }

    pub fn update(&mut self, current_world: [f64; 2]) {
        self.current_world = current_world;
        self.crossing = current_world[0] < self.start_world[0];
    }

    pub fn world_rect(&self) -> WorldRect {
        WorldRect::from_two_points(self.start_world, self.current_world)
    }
}

/// Combined CAD canvas state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadCanvasState {
    pub camera: OrthoCamera2D,
    pub grid: CadGrid,
    pub cursor_world: Option<[f64; 2]>,
    pub rubber_band: Option<RubberBand>,
    /// Snap-indicator overlay (positions are world XY, z=0).
    pub snaps: Vec<SnapHit>,
    /// Entity ID currently hovered, if any.
    pub hovered_entity: Option<u64>,
}

impl CadCanvasState {
    pub fn new(viewport_px: [u32; 2]) -> Self {
        Self {
            camera: OrthoCamera2D::new(viewport_px),
            grid: CadGrid::default(),
            cursor_world: None,
            rubber_band: None,
            snaps: Vec::new(),
            hovered_entity: None,
        }
    }

    pub fn set_cursor_screen(&mut self, p_px: [f64; 2]) {
        self.cursor_world = Some(self.camera.screen_to_world(p_px));
    }

    pub fn begin_rubber_band(&mut self) {
        if let Some(c) = self.cursor_world {
            self.rubber_band = Some(RubberBand::new(c));
        }
    }

    pub fn update_rubber_band(&mut self) {
        if let (Some(rb), Some(c)) = (self.rubber_band.as_mut(), self.cursor_world) {
            rb.update(c);
        }
    }

    pub fn end_rubber_band(&mut self) -> Option<RubberBand> {
        self.rubber_band.take()
    }

    pub fn clear_snaps(&mut self) {
        self.snaps.clear();
    }

    pub fn add_snap(&mut self, kind: SnapKind, p: [f64; 2]) {
        self.snaps.push(SnapHit {
            kind,
            position: [p[0] as f32, p[1] as f32, 0.0],
            distance: 0.0,
        });
    }

    pub fn set_hovered_entity(&mut self, entity: Option<u64>) {
        self.hovered_entity = entity;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_world_roundtrip_at_origin() {
        let cam = OrthoCamera2D::new([1000, 800]);
        let p_px = [500.0, 400.0];
        let w = cam.screen_to_world(p_px);
        assert!((w[0]).abs() < 1e-9);
        assert!((w[1]).abs() < 1e-9);
        let back = cam.world_to_screen(w);
        assert!((back[0] - p_px[0]).abs() < 1e-6);
        assert!((back[1] - p_px[1]).abs() < 1e-6);
    }

    #[test]
    fn screen_to_world_y_inverted() {
        let cam = OrthoCamera2D::new([200, 100]);
        // Pixel above centre should map to positive y.
        let w_top = cam.screen_to_world([100.0, 0.0]);
        let w_bot = cam.screen_to_world([100.0, 100.0]);
        assert!(w_top[1] > 0.0);
        assert!(w_bot[1] < 0.0);
    }

    #[test]
    fn pan_pixels_moves_centre_in_opposite_direction() {
        let mut cam = OrthoCamera2D::new([1000, 1000]);
        cam.pan_pixels(100.0, 0.0);
        // Pan right by 100 px → centre shifts left by 100 world units.
        assert!((cam.center[0] + 100.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_about_cursor_keeps_cursor_world_fixed() {
        let mut cam = OrthoCamera2D::new([1000, 1000]);
        let cursor = [800.0, 200.0];
        let before = cam.screen_to_world(cursor);
        cam.zoom_about(0.5, Some(cursor));
        let after = cam.screen_to_world(cursor);
        assert!((before[0] - after[0]).abs() < 1e-6);
        assert!((before[1] - after[1]).abs() < 1e-6);
        // And units-per-pixel did actually shrink (zoomed in).
        assert!(cam.units_per_pixel < 1.0);
    }

    #[test]
    fn fit_to_rect_centres_and_scales() {
        let mut cam = OrthoCamera2D::new([1000, 1000]);
        let rect = WorldRect::from_two_points([-500.0, -500.0], [500.0, 500.0]);
        cam.fit_to_rect(rect, 1.0);
        assert!((cam.center[0]).abs() < 1e-9);
        assert!((cam.center[1]).abs() < 1e-9);
        assert!((cam.units_per_pixel - 1.0).abs() < 1e-6);
    }

    #[test]
    fn visible_world_rect_matches_viewport_world_size() {
        let cam = OrthoCamera2D {
            center: [100.0, 50.0],
            units_per_pixel: 2.0,
            viewport_px: [200, 100],
        };
        let rect = cam.visible_world_rect();
        assert!((rect.width() - 400.0).abs() < 1e-9);
        assert!((rect.height() - 200.0).abs() < 1e-9);
        assert!((rect.min[0] - (-100.0)).abs() < 1e-9);
        assert!((rect.max[1] - 150.0).abs() < 1e-9);
    }

    #[test]
    fn world_rect_contains_and_intersects() {
        let a = WorldRect::from_two_points([0.0, 0.0], [10.0, 10.0]);
        let b = WorldRect::from_two_points([5.0, 5.0], [15.0, 15.0]);
        let c = WorldRect::from_two_points([20.0, 20.0], [30.0, 30.0]);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
        assert!(a.contains([5.0, 5.0]));
        assert!(!a.contains([11.0, 11.0]));
    }

    #[test]
    fn grid_produces_expected_line_counts_for_unit_viewport() {
        // 1000×1000 pixel viewport at 1 unit/px ⇒ visible 1000×1000
        // world units; major at 100 → 11 major lines; subdivisions=4
        // → minor at 25 units = 25 px (well over the 4-px threshold).
        let cam = OrthoCamera2D {
            center: [0.0, 0.0],
            units_per_pixel: 1.0,
            viewport_px: [1000, 1000],
        };
        let rect = cam.visible_world_rect();
        let grid = CadGrid {
            major_spacing: 100.0,
            subdivisions: 4,
            visible: true,
        };
        let g = compute_grid(&rect, &grid, &cam);
        assert_eq!(g.major_vertical.len(), 11);
        assert_eq!(g.major_horizontal.len(), 11);
        // 3 minor lines per major-cell × ~10 cells ≈ 30, but we only
        // emit those inside the visible rect.
        assert!(g.minor_vertical.len() > 20);
    }

    #[test]
    fn grid_drops_minor_when_too_dense() {
        // Same setup but zoomed out so 25-unit minor spacing < 4 px.
        let cam = OrthoCamera2D {
            center: [0.0, 0.0],
            units_per_pixel: 10.0,
            viewport_px: [100, 100],
        };
        let rect = cam.visible_world_rect();
        let grid = CadGrid {
            major_spacing: 100.0,
            subdivisions: 4,
            visible: true,
        };
        let g = compute_grid(&rect, &grid, &cam);
        assert!(g.minor_vertical.is_empty());
    }

    #[test]
    fn grid_invisible_when_disabled() {
        let cam = OrthoCamera2D::new([200, 200]);
        let rect = cam.visible_world_rect();
        let grid = CadGrid {
            major_spacing: 100.0,
            subdivisions: 10,
            visible: false,
        };
        let g = compute_grid(&rect, &grid, &cam);
        assert_eq!(g.total(), 0);
    }

    #[test]
    fn grid_auto_scales_when_zoomed_out() {
        let mut cam = OrthoCamera2D::new([100, 100]);
        cam.units_per_pixel = 1000.0; // very zoomed out
        let rect = cam.visible_world_rect();
        let grid = CadGrid {
            major_spacing: 100.0,
            subdivisions: 10,
            visible: true,
        };
        let g = compute_grid(&rect, &grid, &cam);
        // At this scale the original 100-unit grid would produce
        // thousands of lines; we cap and step up.
        assert!(g.major_vertical.len() < 200);
    }

    #[test]
    fn crosshair_extends_to_rect_edges() {
        let rect = WorldRect::from_two_points([-100.0, -100.0], [100.0, 100.0]);
        let c = compute_crosshair([10.0, 20.0], &rect);
        assert_eq!(c.horizontal[0], [-100.0, 20.0]);
        assert_eq!(c.horizontal[1], [100.0, 20.0]);
        assert_eq!(c.vertical[0], [10.0, -100.0]);
        assert_eq!(c.vertical[1], [10.0, 100.0]);
    }

    #[test]
    fn rubber_band_detects_crossing_left_to_right_vs_right_to_left() {
        let mut rb = RubberBand::new([100.0, 100.0]);
        rb.update([200.0, 200.0]);
        assert!(!rb.crossing);
        let mut rb = RubberBand::new([200.0, 200.0]);
        rb.update([100.0, 100.0]);
        assert!(rb.crossing);
    }

    #[test]
    fn cad_canvas_state_pipeline() {
        let mut s = CadCanvasState::new([1000, 1000]);
        s.set_cursor_screen([500.0, 500.0]);
        s.begin_rubber_band();
        s.set_cursor_screen([700.0, 300.0]);
        s.update_rubber_band();
        let rb = s.end_rubber_band().unwrap();
        let r = rb.world_rect();
        assert!(r.width() > 0.0);
    }

    #[test]
    fn add_snap_populates_overlay() {
        let mut s = CadCanvasState::new([400, 300]);
        s.add_snap(SnapKind::Endpoint, [10.0, 20.0]);
        s.add_snap(SnapKind::Midpoint, [30.0, 40.0]);
        assert_eq!(s.snaps.len(), 2);
        s.clear_snaps();
        assert!(s.snaps.is_empty());
    }

    #[test]
    fn hovered_entity_can_be_set_and_cleared() {
        let mut s = CadCanvasState::new([400, 300]);
        assert!(s.hovered_entity.is_none());
        s.set_hovered_entity(Some(42));
        assert_eq!(s.hovered_entity, Some(42));
        s.set_hovered_entity(None);
        assert!(s.hovered_entity.is_none());
    }
}
