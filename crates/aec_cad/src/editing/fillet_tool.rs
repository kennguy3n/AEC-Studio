//! Fillet tool — insert a tangent arc between two intersecting lines.
//!
//! The user supplies a radius. We compute the arc centre, find the two
//! tangent points on each source line, shorten each line to its tangent
//! point, and emit an arc connecting them.

use crate::primitives::{Arc, Line};

pub struct FilletTool;

pub struct FilletResult {
    pub line_a: Line,
    pub line_b: Line,
    pub arc: Arc,
}

fn unit(v: [f64; 2]) -> [f64; 2] {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if l < f64::EPSILON {
        return [0.0, 0.0];
    }
    [v[0] / l, v[1] / l]
}

fn corner_direction(line: &Line, corner: [f64; 2]) -> [f64; 2] {
    // Direction *away from* the corner (towards the far endpoint).
    let away =
        if (line.start[0] - corner[0]).abs() < 1e-9 && (line.start[1] - corner[1]).abs() < 1e-9 {
            line.end
        } else {
            line.start
        };
    unit([away[0] - corner[0], away[1] - corner[1]])
}

impl FilletTool {
    /// Fillet two coterminal lines (sharing a common endpoint `corner`)
    /// with a tangent arc of the given radius.
    pub fn fillet_lines(a: &Line, b: &Line, corner: [f64; 2], radius: f64) -> Option<FilletResult> {
        if radius <= 0.0 {
            return None;
        }
        let da = corner_direction(a, corner);
        let db = corner_direction(b, corner);
        let dot = da[0] * db[0] + da[1] * db[1];
        let cross = da[0] * db[1] - da[1] * db[0];
        let theta = (cross.abs()).atan2(dot); // angle between rays in [0, π]
        if theta < 1e-6 || (std::f64::consts::PI - theta).abs() < 1e-6 {
            return None;
        }
        let half = theta * 0.5;
        let t = radius / half.tan(); // distance from corner to tangent point
                                     // Tangent points along each ray.
        let tp_a = [corner[0] + da[0] * t, corner[1] + da[1] * t];
        let tp_b = [corner[0] + db[0] * t, corner[1] + db[1] * t];
        // Arc centre is along the bisector at distance r/sin(half).
        let bisector = unit([da[0] + db[0], da[1] + db[1]]);
        let center_dist = radius / half.sin();
        let center = [
            corner[0] + bisector[0] * center_dist,
            corner[1] + bisector[1] * center_dist,
        ];
        // Build clipped lines (preserve far endpoints).
        let line_a = clip_to(a, corner, tp_a);
        let line_b = clip_to(b, corner, tp_b);
        // Build arc with proper start/end angles.
        let start_angle = ((tp_a[1] - center[1]).atan2(tp_a[0] - center[0])).to_degrees();
        let end_angle = ((tp_b[1] - center[1]).atan2(tp_b[0] - center[0])).to_degrees();
        let (start_angle, end_angle) = if cross > 0.0 {
            (end_angle, start_angle)
        } else {
            (start_angle, end_angle)
        };
        let arc = Arc {
            layer: a.layer.clone(),
            center,
            radius,
            start_angle: start_angle.rem_euclid(360.0),
            end_angle: end_angle.rem_euclid(360.0),
        };
        Some(FilletResult {
            line_a,
            line_b,
            arc,
        })
    }
}

fn clip_to(line: &Line, corner: [f64; 2], tangent_point: [f64; 2]) -> Line {
    let mut new = line.clone();
    let d_start = (line.start[0] - corner[0]).powi(2) + (line.start[1] - corner[1]).powi(2);
    let d_end = (line.end[0] - corner[0]).powi(2) + (line.end[1] - corner[1]).powi(2);
    if d_start < d_end {
        new.start = tangent_point;
    } else {
        new.end = tangent_point;
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fillet_right_angle_radius_one_yields_unit_arc() {
        // Two lines meeting at origin: along +X and along +Y.
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [0.0, 0.0], [0.0, 10.0]);
        let r = FilletTool::fillet_lines(&a, &b, [0.0, 0.0], 1.0).unwrap();
        // Tangent points should be (1,0) and (0,1).
        assert!((r.line_a.start[0] - 1.0).abs() < 1e-6);
        assert!((r.line_b.start[1] - 1.0).abs() < 1e-6);
        // Arc centre = (1,1), radius = 1.
        assert!((r.arc.center[0] - 1.0).abs() < 1e-6);
        assert!((r.arc.center[1] - 1.0).abs() < 1e-6);
        assert!((r.arc.radius - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fillet_parallel_lines_returns_none() {
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        assert!(FilletTool::fillet_lines(&a, &b, [0.0, 0.0], 1.0).is_none());
    }
}
