//! Transform gizmo (translate / rotate / scale).

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::camera::Ray;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GizmoAxis {
    X,
    Y,
    Z,
    XY,
    XZ,
    YZ,
    Screen,
}

impl GizmoAxis {
    pub fn direction(self) -> Vec3 {
        match self {
            GizmoAxis::X => Vec3::X,
            GizmoAxis::Y => Vec3::Y,
            GizmoAxis::Z => Vec3::Z,
            GizmoAxis::XY => Vec3::new(1.0, 1.0, 0.0).normalize(),
            GizmoAxis::XZ => Vec3::new(1.0, 0.0, 1.0).normalize(),
            GizmoAxis::YZ => Vec3::new(0.0, 1.0, 1.0).normalize(),
            GizmoAxis::Screen => Vec3::Z,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformGizmo {
    pub mode: GizmoMode,
    pub origin: [f32; 3],
    pub size_mm: f32,
}

impl TransformGizmo {
    pub fn new(mode: GizmoMode, origin: Vec3) -> Self {
        Self {
            mode,
            origin: [origin.x, origin.y, origin.z],
            size_mm: 800.0,
        }
    }

    /// Pick an axis handle from a ray. Returns the axis whose handle (a
    /// short segment of length `size_mm` starting at `origin` in the axis
    /// direction) lies closest to the ray, within `pick_radius_mm`.
    pub fn pick_axis(&self, ray: &Ray, pick_radius_mm: f32) -> Option<GizmoAxis> {
        let origin: Vec3 = self.origin.into();
        let candidates = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z];
        let mut best: Option<(GizmoAxis, f32)> = None;
        for axis in candidates {
            let dir = axis.direction();
            let d = ray_to_segment_distance(ray, origin, origin + dir * self.size_mm);
            if d <= pick_radius_mm {
                match &best {
                    Some((_, bd)) if d >= *bd => {}
                    _ => best = Some((axis, d)),
                }
            }
        }
        best.map(|(a, _)| a)
    }
}

/// Minimum distance between a ray (infinite, one-sided) and a finite line
/// segment `[a, b]`. Used by gizmo axis picking.
fn ray_to_segment_distance(ray: &Ray, a: Vec3, b: Vec3) -> f32 {
    let seg = b - a;
    let seg_len_sq = seg.length_squared();
    if seg_len_sq < 1e-6 {
        // Segment degenerates to a point: distance from point to ray.
        let to_a = a - ray.origin;
        let t = to_a.dot(ray.direction);
        let closest = ray.origin + ray.direction * t.max(0.0);
        return (closest - a).length();
    }
    // Parametrise both lines and minimise. r(t) = ro + rd*t (t >= 0)
    // s(u) = a + sd*u (0 <= u <= 1). Solve via standard skew-line formula
    // then clamp `u` to the segment and `t` to the half-line.
    let rd = ray.direction;
    let sd = seg;
    let ro = ray.origin;
    let r_dot_s = rd.dot(sd);
    let r_dot_r = rd.dot(rd);
    let s_dot_s = seg_len_sq;
    let diff = ro - a;
    let r_dot_diff = rd.dot(diff);
    let s_dot_diff = sd.dot(diff);
    let denom = r_dot_r * s_dot_s - r_dot_s * r_dot_s;
    if denom.abs() < 1e-6 {
        // Lines are parallel — perpendicular distance is constant. Pick
        // any segment endpoint, project onto the ray, and measure.
        let rd_unit = rd / r_dot_r.sqrt();
        let perp = diff - rd_unit * diff.dot(rd_unit);
        return perp.length();
    }
    let t = (r_dot_s * s_dot_diff - s_dot_s * r_dot_diff) / denom;
    let u = ((r_dot_r * s_dot_diff - r_dot_s * r_dot_diff) / denom).clamp(0.0, 1.0);
    let t = t.max(0.0);
    let p_ray = ro + rd * t;
    let p_seg = a + sd * u;
    (p_ray - p_seg).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_x_axis_when_ray_grazes_x_handle() {
        // Ray runs parallel to X, 50mm above the X axis handle, *between* its
        // start and end. The other axis segments are far further away from
        // the ray, so X should win.
        let gizmo = TransformGizmo::new(GizmoMode::Translate, Vec3::ZERO);
        let ray = Ray {
            origin: Vec3::new(400.0, 50.0, 0.0),
            direction: Vec3::new(1.0, 0.0, 0.0),
        };
        let axis = gizmo.pick_axis(&ray, 80.0).unwrap();
        assert_eq!(axis, GizmoAxis::X);
    }

    #[test]
    fn returns_none_when_ray_far_from_all_axes() {
        let gizmo = TransformGizmo::new(GizmoMode::Translate, Vec3::ZERO);
        let ray = Ray {
            origin: Vec3::new(10_000.0, 10_000.0, 10_000.0),
            direction: Vec3::new(1.0, 0.0, 0.0),
        };
        assert!(gizmo.pick_axis(&ray, 50.0).is_none());
    }
}
