//! Circle primitive.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub layer: String,
    pub center: [f64; 2],
    pub radius: f64,
}

impl Circle {
    pub fn new(layer: impl Into<String>, center: [f64; 2], radius: f64) -> Self {
        Self {
            layer: layer.into(),
            center,
            radius: radius.max(0.0),
        }
    }

    pub fn circumference(&self) -> f64 {
        std::f64::consts::TAU * self.radius
    }

    pub fn area(&self) -> f64 {
        std::f64::consts::PI * self.radius * self.radius
    }

    pub fn quadrant_points(&self) -> [[f64; 2]; 4] {
        [
            [self.center[0] + self.radius, self.center[1]],
            [self.center[0], self.center[1] + self.radius],
            [self.center[0] - self.radius, self.center[1]],
            [self.center[0], self.center[1] - self.radius],
        ]
    }
}

impl Drawable for Circle {
    fn bbox(&self) -> Bbox {
        Bbox {
            min: [self.center[0] - self.radius, self.center[1] - self.radius],
            max: [self.center[0] + self.radius, self.center[1] + self.radius],
        }
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Circle {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let dx = point[0] - self.center[0];
        let dy = point[1] - self.center[1];
        let d = (dx * dx + dy * dy).sqrt();
        (d - self.radius).powi(2)
    }

    fn inside(&self, window: &Bbox) -> bool {
        let b = self.bbox();
        b.min[0] >= window.min[0]
            && b.min[1] >= window.min[1]
            && b.max[0] <= window.max[0]
            && b.max[1] <= window.max[1]
    }
}

impl Snappable for Circle {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let q = self.quadrant_points();
        vec![
            SnapPoint {
                kind: SnapKind::Center,
                at: self.center,
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: q[0],
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: q[1],
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: q[2],
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: q[3],
            },
        ]
    }
}

impl Transformable for Circle {
    /// Apply an affine transform. A non-uniform scale would turn a circle
    /// into an ellipse, which is not representable here; the editing tools
    /// (`scale_tool`, `mirror_tool`) only emit uniform scales so this is
    /// safe in normal use. When the input scale is non-uniform we fall
    /// back to the mean of the two scale factors as a defensive
    /// approximation rather than picking a single axis arbitrarily.
    fn transformed(&self, t: &Affine2) -> Self {
        let new_center = t.apply(self.center);
        let scale = (t.scale[0].abs() + t.scale[1].abs()) * 0.5;
        Self {
            layer: self.layer.clone(),
            center: new_center,
            radius: self.radius * scale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_and_circumference() {
        let c = Circle::new("0", [0.0, 0.0], 2.0);
        assert!((c.area() - std::f64::consts::PI * 4.0).abs() < 1e-9);
        assert!((c.circumference() - std::f64::consts::TAU * 2.0).abs() < 1e-9);
    }

    #[test]
    fn distance_to_circle_edge() {
        let c = Circle::new("0", [0.0, 0.0], 5.0);
        assert!((c.distance2([7.0, 0.0]).sqrt() - 2.0).abs() < 1e-9);
        assert!((c.distance2([3.0, 0.0]).sqrt() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn snap_points_include_center_and_quadrants() {
        let c = Circle::new("0", [1.0, 1.0], 2.0);
        let snaps = c.snap_points();
        assert_eq!(snaps.len(), 5);
        assert!(snaps.iter().any(|s| s.kind == SnapKind::Center));
        assert_eq!(
            snaps
                .iter()
                .filter(|s| s.kind == SnapKind::Quadrant)
                .count(),
            4
        );
    }

    #[test]
    fn transformed_translation_keeps_radius() {
        let c = Circle::new("0", [0.0, 0.0], 3.0);
        let t = Affine2::translation(2.0, -2.0);
        let c2 = c.transformed(&t);
        assert_eq!(c2.center, [2.0, -2.0]);
        assert!((c2.radius - 3.0).abs() < 1e-9);
    }
}
