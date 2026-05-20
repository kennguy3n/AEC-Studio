//! Line primitive.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub layer: String,
    pub start: [f64; 2],
    pub end: [f64; 2],
    #[serde(default)]
    pub color_override: Option<i16>,
    #[serde(default)]
    pub lineweight_override: Option<i16>,
    #[serde(default)]
    pub linetype_override: Option<String>,
}

impl Line {
    pub fn new(layer: impl Into<String>, start: [f64; 2], end: [f64; 2]) -> Self {
        Self {
            layer: layer.into(),
            start,
            end,
            color_override: None,
            lineweight_override: None,
            linetype_override: None,
        }
    }

    pub fn length(&self) -> f64 {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        (dx * dx + dy * dy).sqrt()
    }

    pub fn midpoint(&self) -> [f64; 2] {
        [
            0.5 * (self.start[0] + self.end[0]),
            0.5 * (self.start[1] + self.end[1]),
        ]
    }

    /// Closest point on the segment to `p` (clamped to endpoints).
    pub fn closest_point(&self, p: [f64; 2]) -> [f64; 2] {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        let len2 = dx * dx + dy * dy;
        if len2 < f64::EPSILON {
            return self.start;
        }
        let t = ((p[0] - self.start[0]) * dx + (p[1] - self.start[1]) * dy) / len2;
        let t = t.clamp(0.0, 1.0);
        [self.start[0] + t * dx, self.start[1] + t * dy]
    }
}

impl Drawable for Line {
    fn bbox(&self) -> Bbox {
        let mut b = Bbox::from_point(self.start);
        b.extend(self.end);
        b
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Line {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let c = self.closest_point(point);
        let dx = point[0] - c[0];
        let dy = point[1] - c[1];
        dx * dx + dy * dy
    }

    fn inside(&self, window: &Bbox) -> bool {
        window.contains(self.start) && window.contains(self.end)
    }
}

impl Snappable for Line {
    fn snap_points(&self) -> Vec<SnapPoint> {
        vec![
            SnapPoint {
                kind: SnapKind::Endpoint,
                at: self.start,
            },
            SnapPoint {
                kind: SnapKind::Endpoint,
                at: self.end,
            },
            SnapPoint {
                kind: SnapKind::Midpoint,
                at: self.midpoint(),
            },
        ]
    }
}

impl Transformable for Line {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            start: t.apply(self.start),
            end: t.apply(self.end),
            color_override: self.color_override,
            lineweight_override: self.lineweight_override,
            linetype_override: self.linetype_override.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_midpoint() {
        let l = Line::new("0", [0.0, 0.0], [3.0, 4.0]);
        assert!((l.length() - 5.0).abs() < 1e-9);
        assert_eq!(l.midpoint(), [1.5, 2.0]);
    }

    #[test]
    fn distance_to_endpoint_and_midpoint() {
        let l = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        assert!((l.distance2([5.0, 1.0]).sqrt() - 1.0).abs() < 1e-9);
        assert!(l.distance2([-1.0, 0.0]).sqrt() - 1.0 < 1e-9);
    }

    #[test]
    fn snap_points_and_transform() {
        let l = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let snaps = l.snap_points();
        assert_eq!(snaps.len(), 3);
        let m = Affine2::translation(1.0, 2.0);
        let lt = l.transformed(&m);
        assert_eq!(lt.start, [1.0, 2.0]);
        assert_eq!(lt.end, [11.0, 2.0]);
    }

    #[test]
    fn serde_roundtrip() {
        let l = Line::new("Walls", [1.5, 2.5], [3.25, 4.0]);
        let s = serde_json::to_string(&l).unwrap();
        let back: Line = serde_json::from_str(&s).unwrap();
        assert_eq!(l, back);
    }
}
