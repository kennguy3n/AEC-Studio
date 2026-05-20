//! Arc primitive — a section of a circle.

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arc {
    pub layer: String,
    pub center: [f64; 2],
    pub radius: f64,
    /// Degrees, counter-clockwise from +X.
    pub start_angle: f64,
    pub end_angle: f64,
}

impl Arc {
    pub fn new(layer: impl Into<String>, center: [f64; 2], radius: f64) -> Self {
        Self {
            layer: layer.into(),
            center,
            radius,
            start_angle: 0.0,
            end_angle: 90.0,
        }
    }

    /// Sweep in degrees, normalised to (0, 360].
    pub fn sweep(&self) -> f64 {
        let s = self.start_angle.rem_euclid(360.0);
        let e = self.end_angle.rem_euclid(360.0);
        let raw = e - s;
        if raw <= 0.0 {
            raw + 360.0
        } else {
            raw
        }
    }

    pub fn length(&self) -> f64 {
        self.radius * self.sweep().to_radians()
    }

    pub fn point_at(&self, angle_deg: f64) -> [f64; 2] {
        let r = angle_deg.to_radians();
        [
            self.center[0] + self.radius * r.cos(),
            self.center[1] + self.radius * r.sin(),
        ]
    }

    pub fn start_point(&self) -> [f64; 2] {
        self.point_at(self.start_angle)
    }

    pub fn end_point(&self) -> [f64; 2] {
        self.point_at(self.end_angle)
    }

    pub fn midpoint(&self) -> [f64; 2] {
        let mid = self.start_angle + self.sweep() / 2.0;
        self.point_at(mid)
    }

    /// Return true if `theta_deg` (any range) falls within this arc's
    /// sweep span (closed interval, modulo 360°).
    pub fn contains_angle(&self, theta_deg: f64) -> bool {
        let s = self.start_angle.rem_euclid(360.0);
        let t = theta_deg.rem_euclid(360.0);
        let delta = (t - s).rem_euclid(360.0);
        delta <= self.sweep() + 1e-9
    }

    pub fn closest_point(&self, p: [f64; 2]) -> [f64; 2] {
        let dx = p[0] - self.center[0];
        let dy = p[1] - self.center[1];
        let r2 = dx * dx + dy * dy;
        if r2 < f64::EPSILON {
            return self.start_point();
        }
        let theta = dy.atan2(dx).to_degrees().rem_euclid(360.0);
        if self.contains_angle(theta) {
            let scale = self.radius / r2.sqrt();
            return [self.center[0] + dx * scale, self.center[1] + dy * scale];
        }
        // Outside the sweep — return the nearer endpoint.
        let s = self.start_point();
        let e = self.end_point();
        let ds = (p[0] - s[0]).powi(2) + (p[1] - s[1]).powi(2);
        let de = (p[0] - e[0]).powi(2) + (p[1] - e[1]).powi(2);
        if ds <= de {
            s
        } else {
            e
        }
    }
}

impl Drawable for Arc {
    fn bbox(&self) -> Bbox {
        // Sample endpoints and any axis quadrants (0°/90°/180°/270°) the
        // sweep crosses — these are the only extrema.
        let mut b = Bbox::from_point(self.start_point());
        b.extend(self.end_point());
        for quad in [0.0, 90.0, 180.0, 270.0] {
            if self.contains_angle(quad) {
                b.extend(self.point_at(quad));
            }
        }
        b
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Arc {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let c = self.closest_point(point);
        (point[0] - c[0]).powi(2) + (point[1] - c[1]).powi(2)
    }

    fn inside(&self, window: &Bbox) -> bool {
        window.contains(self.start_point())
            && window.contains(self.end_point())
            && window.contains(self.midpoint())
    }
}

impl Snappable for Arc {
    fn snap_points(&self) -> Vec<SnapPoint> {
        let mut pts = vec![
            SnapPoint {
                kind: SnapKind::Endpoint,
                at: self.start_point(),
            },
            SnapPoint {
                kind: SnapKind::Endpoint,
                at: self.end_point(),
            },
            SnapPoint {
                kind: SnapKind::Center,
                at: self.center,
            },
            SnapPoint {
                kind: SnapKind::Midpoint,
                at: self.midpoint(),
            },
        ];
        for quad in [0.0, 90.0, 180.0, 270.0] {
            if self.contains_angle(quad) {
                pts.push(SnapPoint {
                    kind: SnapKind::Quadrant,
                    at: self.point_at(quad),
                });
            }
        }
        pts
    }
}

impl Transformable for Arc {
    fn transformed(&self, t: &Affine2) -> Self {
        // Uniform-scale arcs only stay arcs; for non-uniform scale the
        // result is technically an elliptical arc which we do not model
        // here — the editor refuses non-uniform on arcs upstream.
        let new_center = t.apply(self.center);
        // Compute new radius from a point on the arc.
        let s = t.apply(self.start_point());
        let dx = s[0] - new_center[0];
        let dy = s[1] - new_center[1];
        let new_radius = (dx * dx + dy * dy).sqrt();
        // Rotation just shifts both angles by the same delta.
        let new_start = (dy.atan2(dx).to_degrees()).rem_euclid(360.0);
        let new_end = (new_start + self.sweep()).rem_euclid(TAU.to_degrees());
        let new_end = if new_end == 0.0 { 360.0 } else { new_end };
        Self {
            layer: self.layer.clone(),
            center: new_center,
            radius: new_radius,
            start_angle: new_start,
            end_angle: new_end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_and_length() {
        let a = Arc {
            layer: "0".into(),
            center: [0.0, 0.0],
            radius: 10.0,
            start_angle: 0.0,
            end_angle: 90.0,
        };
        assert!((a.sweep() - 90.0).abs() < 1e-9);
        assert!((a.length() - 10.0 * std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn bbox_includes_quadrants_inside_sweep() {
        let a = Arc {
            layer: "0".into(),
            center: [0.0, 0.0],
            radius: 5.0,
            start_angle: 0.0,
            end_angle: 180.0,
        };
        let b = a.bbox();
        assert!((b.max[1] - 5.0).abs() < 1e-9); // 90° quadrant visible
        assert!((b.min[0] + 5.0).abs() < 1e-9); // 180° endpoint
    }

    #[test]
    fn closest_point_on_arc_falls_on_circle_in_sweep() {
        let a = Arc {
            layer: "0".into(),
            center: [0.0, 0.0],
            radius: 5.0,
            start_angle: 0.0,
            end_angle: 90.0,
        };
        let c = a.closest_point([1.0, 1.0]);
        assert!((c[0].powi(2) + c[1].powi(2) - 25.0).abs() < 1e-6);
    }

    #[test]
    fn contains_angle_handles_wraparound() {
        let a = Arc {
            layer: "0".into(),
            center: [0.0, 0.0],
            radius: 1.0,
            start_angle: 350.0,
            end_angle: 10.0,
        };
        assert!(a.contains_angle(0.0));
        assert!(!a.contains_angle(180.0));
    }
}
