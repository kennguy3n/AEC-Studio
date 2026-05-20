//! Behavioural traits implemented by every CAD primitive.
//!
//! These four traits give every primitive a uniform vocabulary for the
//! viewport (`Drawable`), the picking system (`Selectable`), the snap
//! engine (`Snappable`), and the editing tools (`Transformable`).

use serde::{Deserialize, Serialize};

/// Axis-aligned 2D bounding box. Mirrors what the viewport's broad-phase
/// uses for layer culling and what the snap engine uses for proximity
/// filtering. Coordinates are in model space millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bbox {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Bbox {
    pub fn empty() -> Self {
        Self {
            min: [f64::INFINITY, f64::INFINITY],
            max: [f64::NEG_INFINITY, f64::NEG_INFINITY],
        }
    }

    pub fn from_point(p: [f64; 2]) -> Self {
        Self { min: p, max: p }
    }

    pub fn extend(&mut self, p: [f64; 2]) {
        if p[0] < self.min[0] {
            self.min[0] = p[0];
        }
        if p[1] < self.min[1] {
            self.min[1] = p[1];
        }
        if p[0] > self.max[0] {
            self.max[0] = p[0];
        }
        if p[1] > self.max[1] {
            self.max[1] = p[1];
        }
    }

    pub fn union(mut self, other: &Bbox) -> Self {
        self.extend(other.min);
        self.extend(other.max);
        self
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }

    pub fn width(&self) -> f64 {
        (self.max[0] - self.min[0]).max(0.0)
    }

    pub fn height(&self) -> f64 {
        (self.max[1] - self.min[1]).max(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0] || self.min[1] > self.max[1]
    }
}

/// Kind of snap point a primitive can expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapKind {
    Endpoint,
    Midpoint,
    Center,
    Quadrant,
    Node,
    Insertion,
    Nearest,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SnapPoint {
    pub kind: SnapKind,
    pub at: [f64; 2],
}

/// Affine transform with translation+rotation+uniform scale, plus a flip
/// flag for mirror operations. This is what every editing tool produces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine2 {
    pub translate: [f64; 2],
    pub rotation_deg: f64,
    pub scale: [f64; 2],
    /// Origin around which rotation/scale are applied (model space).
    pub pivot: [f64; 2],
}

impl Affine2 {
    pub fn identity() -> Self {
        Self {
            translate: [0.0, 0.0],
            rotation_deg: 0.0,
            scale: [1.0, 1.0],
            pivot: [0.0, 0.0],
        }
    }

    pub fn translation(dx: f64, dy: f64) -> Self {
        Self {
            translate: [dx, dy],
            ..Self::identity()
        }
    }

    pub fn rotation_about(pivot: [f64; 2], deg: f64) -> Self {
        Self {
            rotation_deg: deg,
            pivot,
            ..Self::identity()
        }
    }

    pub fn scaling_about(pivot: [f64; 2], sx: f64, sy: f64) -> Self {
        Self {
            scale: [sx, sy],
            pivot,
            ..Self::identity()
        }
    }

    /// Mirror about an arbitrary line `p` → `q`. Returned as a pre-composed
    /// rotation+flip in the X axis around the line's foot point. Computing
    /// this once and reusing for every vertex is far cheaper than calling
    /// a reflect-point helper per vertex.
    pub fn mirror_through(p: [f64; 2], q: [f64; 2]) -> Self {
        let dx = q[0] - p[0];
        let dy = q[1] - p[1];
        let theta = dy.atan2(dx).to_degrees();
        Self {
            translate: [0.0, 0.0],
            rotation_deg: theta * -2.0,
            // negative Y scale = flip about the X axis after rotating the
            // line to horizontal; the caller composes via `apply_mirror`.
            scale: [1.0, -1.0],
            pivot: p,
        }
    }

    /// Apply the affine to a single 2D point.
    pub fn apply(&self, point: [f64; 2]) -> [f64; 2] {
        // Translate into pivot frame, scale, rotate, translate back, then
        // apply post-translation.
        let px = point[0] - self.pivot[0];
        let py = point[1] - self.pivot[1];
        let sx = px * self.scale[0];
        let sy = py * self.scale[1];
        let rad = self.rotation_deg.to_radians();
        let (sin_t, cos_t) = rad.sin_cos();
        let rx = sx * cos_t - sy * sin_t;
        let ry = sx * sin_t + sy * cos_t;
        [
            rx + self.pivot[0] + self.translate[0],
            ry + self.pivot[1] + self.translate[1],
        ]
    }
}

/// A primitive that the wgpu canvas can draw.
pub trait Drawable {
    /// Bounding box used for layer-level frustum culling and zoom-extents.
    fn bbox(&self) -> Bbox;
    fn layer(&self) -> &str;
}

/// A primitive that can be hit-tested by the pointer / rubber-band.
pub trait Selectable {
    /// Return the squared distance from `point` to the primitive in model
    /// space. The picker chooses the smallest. `f64::INFINITY` = miss.
    fn distance2(&self, point: [f64; 2]) -> f64;

    /// True if the primitive lies fully inside the given window (used by
    /// the rubber-band's window-select). Crossing-selects use `bbox` ∩ window.
    fn inside(&self, window: &Bbox) -> bool;
}

/// A primitive that exposes snap points to the precision engine.
pub trait Snappable {
    fn snap_points(&self) -> Vec<SnapPoint>;
}

/// A primitive that can be transformed (move/rotate/scale/mirror). The
/// returned `Self` is the new entity; the caller wraps it in a command.
pub trait Transformable: Sized {
    fn transformed(&self, t: &Affine2) -> Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_extend_and_union() {
        let mut a = Bbox::empty();
        a.extend([1.0, 2.0]);
        a.extend([3.0, 4.0]);
        let mut b = Bbox::empty();
        b.extend([-1.0, 0.0]);
        let u = a.union(&b);
        assert_eq!(u.min, [-1.0, 0.0]);
        assert_eq!(u.max, [3.0, 4.0]);
        assert!(!u.is_empty());
        assert!(u.contains([0.5, 1.0]));
    }

    #[test]
    fn affine_translation() {
        let t = Affine2::translation(10.0, -5.0);
        assert_eq!(t.apply([0.0, 0.0]), [10.0, -5.0]);
    }

    #[test]
    fn affine_rotation_about_pivot() {
        // 90° CCW about (1,1) sends (2,1) to (1,2).
        let t = Affine2::rotation_about([1.0, 1.0], 90.0);
        let r = t.apply([2.0, 1.0]);
        assert!((r[0] - 1.0).abs() < 1e-9);
        assert!((r[1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn affine_scaling_about_pivot() {
        let t = Affine2::scaling_about([1.0, 1.0], 2.0, 2.0);
        assert_eq!(t.apply([2.0, 2.0]), [3.0, 3.0]);
    }
}
