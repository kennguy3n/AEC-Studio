//! Mirror tool — reflect about an arbitrary line.
//!
//! We use a closed-form 2×2 reflection matrix rather than the
//! rotation+flip encoding in [`Affine2`] because mirror operations have
//! to produce an exact reflection (not a rotation that "happens to" flip
//! a coordinate).

use crate::primitives::traits::Affine2;
use crate::primitives::{
    Arc, Circle, Ellipse, Hatch, HatchBoundary, Line, MText, Polyline, PolylineVertex, Primitive,
    Spline, Text,
};

pub struct MirrorTool;

fn reflect_point(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    if len2 < f64::EPSILON {
        return p;
    }
    // Foot of perpendicular.
    let t = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2;
    let fx = a[0] + t * dx;
    let fy = a[1] + t * dy;
    [2.0 * fx - p[0], 2.0 * fy - p[1]]
}

impl MirrorTool {
    pub fn apply(primitive: &Primitive, a: [f64; 2], b: [f64; 2]) -> Primitive {
        match primitive {
            Primitive::Line(l) => Primitive::Line(Line {
                layer: l.layer.clone(),
                start: reflect_point(l.start, a, b),
                end: reflect_point(l.end, a, b),
                color_override: l.color_override,
                lineweight_override: l.lineweight_override,
                linetype_override: l.linetype_override.clone(),
            }),
            Primitive::Polyline(p) => Primitive::Polyline(Polyline {
                layer: p.layer.clone(),
                vertices: p
                    .vertices
                    .iter()
                    .map(|v| PolylineVertex {
                        at: reflect_point(v.at, a, b),
                        bulge: -v.bulge,
                    })
                    .collect(),
                closed: p.closed,
                elevation: p.elevation,
                color_override: p.color_override,
                lineweight_override: p.lineweight_override,
            }),
            Primitive::Arc(arc) => {
                // Mirror centre and swap start/end angles around the
                // mirror line (CCW becomes CW).
                let new_center = reflect_point(arc.center, a, b);
                let new_start = reflect_point(arc.start_point(), a, b);
                let new_end = reflect_point(arc.end_point(), a, b);
                let start_angle = ((new_end[1] - new_center[1]).atan2(new_end[0] - new_center[0]))
                    .to_degrees()
                    .rem_euclid(360.0);
                let end_angle = ((new_start[1] - new_center[1])
                    .atan2(new_start[0] - new_center[0]))
                .to_degrees()
                .rem_euclid(360.0);
                Primitive::Arc(Arc {
                    layer: arc.layer.clone(),
                    center: new_center,
                    radius: arc.radius,
                    start_angle,
                    end_angle,
                })
            }
            Primitive::Circle(c) => Primitive::Circle(Circle::new(
                c.layer.clone(),
                reflect_point(c.center, a, b),
                c.radius,
            )),
            Primitive::Ellipse(e) => {
                let new_center = reflect_point(e.center, a, b);
                let major_end =
                    reflect_point([e.center[0] + e.major[0], e.center[1] + e.major[1]], a, b);
                Primitive::Ellipse(Ellipse {
                    layer: e.layer.clone(),
                    center: new_center,
                    major: [major_end[0] - new_center[0], major_end[1] - new_center[1]],
                    ratio: e.ratio,
                    start_param: e.start_param,
                    end_param: e.end_param,
                })
            }
            Primitive::Spline(s) => Primitive::Spline(Spline {
                layer: s.layer.clone(),
                degree: s.degree,
                control_points: s
                    .control_points
                    .iter()
                    .map(|&p| reflect_point(p, a, b))
                    .collect(),
                knots: s.knots.clone(),
                closed: s.closed,
            }),
            Primitive::Hatch(h) => Primitive::Hatch(Hatch {
                layer: h.layer.clone(),
                boundary_loops: h
                    .boundary_loops
                    .iter()
                    .map(|loop_| HatchBoundary {
                        vertices: loop_
                            .vertices
                            .iter()
                            .map(|&v| reflect_point(v, a, b))
                            .collect(),
                    })
                    .collect(),
                pattern: h.pattern.clone(),
                scale: h.scale,
                angle: -h.angle,
            }),
            Primitive::Text(t) => Primitive::Text(Text {
                layer: t.layer.clone(),
                position: reflect_point(t.position, a, b),
                height: t.height,
                rotation_deg: -t.rotation_deg,
                style: t.style.clone(),
                content: t.content.clone(),
                h_align: t.h_align,
                v_align: t.v_align,
                width_factor: t.width_factor,
                oblique_angle_deg: t.oblique_angle_deg,
            }),
            Primitive::MText(m) => Primitive::MText(MText {
                layer: m.layer.clone(),
                position: reflect_point(m.position, a, b),
                height: m.height,
                width: m.width,
                rotation_deg: -m.rotation_deg,
                content: m.content.clone(),
                style: m.style.clone(),
                line_spacing: m.line_spacing,
            }),
        }
    }
}

// Suppress unused import warning when not all tools use Affine2.
#[allow(dead_code)]
fn _ensure_affine2_in_scope(_t: Affine2) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Line;

    #[test]
    fn mirror_about_x_axis_flips_y() {
        let l = Primitive::Line(Line::new("0", [1.0, 2.0], [3.0, 4.0]));
        let m = MirrorTool::apply(&l, [0.0, 0.0], [1.0, 0.0]);
        if let Primitive::Line(line) = m {
            assert!((line.start[0] - 1.0).abs() < 1e-9);
            assert!((line.start[1] + 2.0).abs() < 1e-9);
            assert!((line.end[1] + 4.0).abs() < 1e-9);
        }
    }

    #[test]
    fn mirror_about_y_axis_flips_x() {
        let l = Primitive::Line(Line::new("0", [1.0, 2.0], [3.0, 4.0]));
        let m = MirrorTool::apply(&l, [0.0, 0.0], [0.0, 1.0]);
        if let Primitive::Line(line) = m {
            assert!((line.start[0] + 1.0).abs() < 1e-9);
            assert!((line.end[0] + 3.0).abs() < 1e-9);
        }
    }
}
