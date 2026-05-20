//! Ellipse primitive — DXF semantics: a centre, a major-axis offset, a
//! ratio of minor:major, and a parametric start/end interval.

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ellipse {
    pub layer: String,
    pub center: [f64; 2],
    /// Major-axis end relative to centre.
    pub major: [f64; 2],
    /// Minor / major ratio in (0, 1].
    pub ratio: f64,
    /// Parametric start angle (radians).
    pub start_param: f64,
    /// Parametric end angle (radians).
    pub end_param: f64,
}

impl Ellipse {
    pub fn new(layer: impl Into<String>, center: [f64; 2], major: [f64; 2], ratio: f64) -> Self {
        Self {
            layer: layer.into(),
            center,
            major,
            ratio: ratio.clamp(1e-6, 1.0),
            start_param: 0.0,
            end_param: TAU,
        }
    }

    pub fn major_radius(&self) -> f64 {
        (self.major[0] * self.major[0] + self.major[1] * self.major[1]).sqrt()
    }

    pub fn minor_radius(&self) -> f64 {
        self.major_radius() * self.ratio
    }

    pub fn major_angle(&self) -> f64 {
        self.major[1].atan2(self.major[0])
    }

    /// Sample the ellipse boundary at parameter `t` (radians on the unit
    /// circle in the ellipse's local frame).
    pub fn point_at(&self, t: f64) -> [f64; 2] {
        let a = self.major_radius();
        let b = self.minor_radius();
        let theta = self.major_angle();
        let (sin_th, cos_th) = theta.sin_cos();
        let (sin_t, cos_t) = t.sin_cos();
        let local_x = a * cos_t;
        let local_y = b * sin_t;
        [
            self.center[0] + local_x * cos_th - local_y * sin_th,
            self.center[1] + local_x * sin_th + local_y * cos_th,
        ]
    }

    pub fn sweep(&self) -> f64 {
        let raw = self.end_param - self.start_param;
        if raw <= 0.0 {
            raw + TAU
        } else {
            raw
        }
    }
}

impl Drawable for Ellipse {
    fn bbox(&self) -> Bbox {
        // Closed-form extrema along the rotated major/minor axes.
        let theta = self.major_angle();
        let a = self.major_radius();
        let b = self.minor_radius();
        // tan(t_extreme_x) = -b * tan(theta) / a → use ±.
        // We just sample 16 points along the sweep + endpoints (cheap,
        // bounded, and good enough for a viewport bbox).
        let mut bbox = Bbox::from_point(self.center);
        let n = 16;
        for i in 0..=n {
            let t = self.start_param + self.sweep() * (i as f64 / n as f64);
            bbox.extend(self.point_at(t));
        }
        let _ = (theta, a, b);
        bbox
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Ellipse {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        // Convert into local frame, scale Y by 1/ratio, find closest on
        // unit-circle (in scaled frame), invert. This is the standard
        // first-order approximation. It is not the exact closest point
        // for ellipses (which would require a quartic root solve), but
        // is good enough for hover/pick at typical screen scales.
        let theta = self.major_angle();
        let (sin_th, cos_th) = theta.sin_cos();
        let dx = point[0] - self.center[0];
        let dy = point[1] - self.center[1];
        let local_x = dx * cos_th + dy * sin_th;
        let local_y = -dx * sin_th + dy * cos_th;
        let a = self.major_radius();
        let b = self.minor_radius();
        let denom = (local_x / a).powi(2) + (local_y / b).powi(2);
        if denom < f64::EPSILON {
            return self.major_radius().powi(2);
        }
        let scale = 1.0 / denom.sqrt();
        let cx = local_x * scale;
        let cy = local_y * scale;
        let dx2 = local_x - cx;
        let dy2 = local_y - cy;
        dx2 * dx2 + dy2 * dy2
    }

    fn inside(&self, window: &Bbox) -> bool {
        let b = self.bbox();
        window.contains(b.min) && window.contains(b.max)
    }
}

impl Snappable for Ellipse {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let theta = self.major_angle();
        let a = self.major_radius();
        let b = self.minor_radius();
        let (sin_th, cos_th) = theta.sin_cos();
        let major_end = [self.center[0] + a * cos_th, self.center[1] + a * sin_th];
        let minor_end = [self.center[0] - b * sin_th, self.center[1] + b * cos_th];
        vec![
            SnapPoint {
                kind: SnapKind::Center,
                at: self.center,
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: major_end,
            },
            SnapPoint {
                kind: SnapKind::Quadrant,
                at: minor_end,
            },
        ]
    }
}

impl Transformable for Ellipse {
    fn transformed(&self, t: &Affine2) -> Self {
        let center = t.apply(self.center);
        // For rotations/translations the major vector rotates with the
        // affine; we apply the affine to the major-axis endpoint relative
        // to the new centre.
        let major_end = t.apply([
            self.center[0] + self.major[0],
            self.center[1] + self.major[1],
        ]);
        Self {
            layer: self.layer.clone(),
            center,
            major: [major_end[0] - center[0], major_end[1] - center[1]],
            ratio: self.ratio,
            start_param: self.start_param,
            end_param: self.end_param,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_minor_radii() {
        let e = Ellipse::new("0", [0.0, 0.0], [3.0, 0.0], 0.5);
        assert!((e.major_radius() - 3.0).abs() < 1e-9);
        assert!((e.minor_radius() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn point_at_param_zero_is_major_end() {
        let e = Ellipse::new("0", [1.0, 2.0], [3.0, 0.0], 0.5);
        let p = e.point_at(0.0);
        assert!((p[0] - 4.0).abs() < 1e-9);
        assert!((p[1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn bbox_includes_major_endpoint() {
        let e = Ellipse::new("0", [0.0, 0.0], [3.0, 0.0], 0.5);
        let b = e.bbox();
        assert!(b.contains([3.0, 0.0]));
        assert!(b.contains([0.0, 1.5]));
    }
}
