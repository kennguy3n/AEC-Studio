//! Offset tool — parallel offset of lines, polylines, arcs, circles.
//!
//! Positive distance = left of the source direction (CCW normal).

use crate::primitives::{Arc, Circle, Line, Polyline, PolylineVertex, Primitive};

pub struct OffsetTool;

fn unit_normal(p1: [f64; 2], p2: [f64; 2]) -> [f64; 2] {
    let dx = p2[0] - p1[0];
    let dy = p2[1] - p1[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len < f64::EPSILON {
        return [0.0, 0.0];
    }
    [-dy / len, dx / len]
}

/// Intersect two infinite lines defined by point+direction. Returns None
/// if (nearly) parallel.
fn ray_intersect(p1: [f64; 2], d1: [f64; 2], p2: [f64; 2], d2: [f64; 2]) -> Option<[f64; 2]> {
    let det = d1[0] * d2[1] - d1[1] * d2[0];
    if det.abs() < 1e-9 {
        return None;
    }
    let dx = p2[0] - p1[0];
    let dy = p2[1] - p1[1];
    let t = (dx * d2[1] - dy * d2[0]) / det;
    Some([p1[0] + t * d1[0], p1[1] + t * d1[1]])
}

impl OffsetTool {
    pub fn offset_line(line: &Line, distance: f64) -> Line {
        let n = unit_normal(line.start, line.end);
        Line {
            layer: line.layer.clone(),
            start: [
                line.start[0] + n[0] * distance,
                line.start[1] + n[1] * distance,
            ],
            end: [line.end[0] + n[0] * distance, line.end[1] + n[1] * distance],
            color_override: line.color_override,
            lineweight_override: line.lineweight_override,
            linetype_override: line.linetype_override.clone(),
        }
    }

    pub fn offset_circle(c: &Circle, distance: f64) -> Option<Circle> {
        let r = c.radius + distance;
        if r <= 0.0 {
            None
        } else {
            Some(Circle::new(c.layer.clone(), c.center, r))
        }
    }

    pub fn offset_arc(arc: &Arc, distance: f64) -> Option<Arc> {
        let r = arc.radius + distance;
        if r <= 0.0 {
            None
        } else {
            Some(Arc {
                layer: arc.layer.clone(),
                center: arc.center,
                radius: r,
                start_angle: arc.start_angle,
                end_angle: arc.end_angle,
            })
        }
    }

    /// Offset an open or closed polyline. Sharp corners are joined via
    /// segment intersection; parallel/collinear segments are joined by a
    /// simple endpoint extrapolation.
    pub fn offset_polyline(pl: &Polyline, distance: f64) -> Polyline {
        let n = pl.vertices.len();
        if n < 2 {
            return pl.clone();
        }
        // Build offset segments first.
        let mut offset_segs = Vec::with_capacity(n.saturating_sub(1));
        let count = if pl.closed { n } else { n - 1 };
        for i in 0..count {
            let a = pl.vertices[i].at;
            let b = pl.vertices[(i + 1) % n].at;
            let normal = unit_normal(a, b);
            offset_segs.push((
                [a[0] + normal[0] * distance, a[1] + normal[1] * distance],
                [b[0] + normal[0] * distance, b[1] + normal[1] * distance],
            ));
        }
        // Stitch consecutive segments by intersecting their lines.
        let mut new_vertices: Vec<PolylineVertex> = Vec::with_capacity(n);
        if pl.closed {
            let last = offset_segs[count - 1];
            for (i, seg) in offset_segs.iter().enumerate() {
                let prev = if i == 0 { last } else { offset_segs[i - 1] };
                let d1 = [prev.1[0] - prev.0[0], prev.1[1] - prev.0[1]];
                let d2 = [seg.1[0] - seg.0[0], seg.1[1] - seg.0[1]];
                let join = ray_intersect(prev.0, d1, seg.0, d2).unwrap_or(seg.0);
                new_vertices.push(PolylineVertex {
                    at: join,
                    bulge: pl.vertices[i].bulge,
                });
            }
        } else {
            // Open polyline: first vertex is the start of seg 0; last is the end of seg N-1.
            new_vertices.push(PolylineVertex {
                at: offset_segs[0].0,
                bulge: pl.vertices[0].bulge,
            });
            for i in 1..count {
                let prev = offset_segs[i - 1];
                let curr = offset_segs[i];
                let d1 = [prev.1[0] - prev.0[0], prev.1[1] - prev.0[1]];
                let d2 = [curr.1[0] - curr.0[0], curr.1[1] - curr.0[1]];
                let join = ray_intersect(prev.0, d1, curr.0, d2).unwrap_or(curr.0);
                new_vertices.push(PolylineVertex {
                    at: join,
                    bulge: pl.vertices[i].bulge,
                });
            }
            new_vertices.push(PolylineVertex {
                at: offset_segs.last().unwrap().1,
                bulge: 0.0,
            });
        }
        Polyline {
            layer: pl.layer.clone(),
            vertices: new_vertices,
            closed: pl.closed,
            elevation: pl.elevation,
            color_override: pl.color_override,
            lineweight_override: pl.lineweight_override,
        }
    }

    pub fn apply(primitive: &Primitive, distance: f64) -> Option<Primitive> {
        match primitive {
            Primitive::Line(l) => Some(Primitive::Line(Self::offset_line(l, distance))),
            Primitive::Circle(c) => Self::offset_circle(c, distance).map(Primitive::Circle),
            Primitive::Arc(a) => Self::offset_arc(a, distance).map(Primitive::Arc),
            Primitive::Polyline(p) => Some(Primitive::Polyline(Self::offset_polyline(p, distance))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_horizontal_line_up_by_one() {
        let l = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let off = OffsetTool::offset_line(&l, 1.0);
        assert!((off.start[1] - 1.0).abs() < 1e-9);
        assert!((off.end[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn offset_horizontal_line_down_by_one() {
        let l = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let off = OffsetTool::offset_line(&l, -1.0);
        assert!((off.start[1] + 1.0).abs() < 1e-9);
    }

    #[test]
    fn offset_circle_grows_radius() {
        let c = Circle::new("0", [0.0, 0.0], 5.0);
        let off = OffsetTool::offset_circle(&c, 1.5).unwrap();
        assert!((off.radius - 6.5).abs() < 1e-9);
    }

    #[test]
    fn offset_inward_collapse_returns_none() {
        let c = Circle::new("0", [0.0, 0.0], 1.0);
        assert!(OffsetTool::offset_circle(&c, -2.0).is_none());
    }

    #[test]
    fn offset_polyline_right_angle_corner() {
        // L-shape: (0,0)-(10,0)-(10,10)
        let pl = Polyline::new(
            "0",
            vec![
                PolylineVertex::new([0.0, 0.0]),
                PolylineVertex::new([10.0, 0.0]),
                PolylineVertex::new([10.0, 10.0]),
            ],
        );
        let off = OffsetTool::offset_polyline(&pl, 1.0);
        // The middle vertex should now be at (9,1) for offset toward
        // the inside (CCW normal of the path).
        let mid = off.vertices[1].at;
        assert!((mid[0] - 9.0).abs() < 1e-6);
        assert!((mid[1] - 1.0).abs() < 1e-6);
    }
}
