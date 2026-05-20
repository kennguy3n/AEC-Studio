//! Extend tool — lengthen a line to its intersection with a boundary
//! entity. Solves the intersection on the line's parametric form so we
//! don't introduce the precision loss of "extend the segment by 1e6 and
//! intersect".

use crate::primitives::{Circle, Line, Primitive};

pub struct ExtendTool;

fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// Line as `p = start + t * dir`, where `dir = end - start` (so `t=0`
/// is start, `t=1` is end). Returns the `t` value of the intersection
/// with the infinite line through `b.start`/`b.end`, or `None` if the
/// lines are parallel.
fn parametric_intersection_with_line(source: &Line, boundary: &Line) -> Option<f64> {
    let p1 = source.start;
    let dir1 = [
        source.end[0] - source.start[0],
        source.end[1] - source.start[1],
    ];
    let p2 = boundary.start;
    let dir2 = [
        boundary.end[0] - boundary.start[0],
        boundary.end[1] - boundary.start[1],
    ];
    let det = dir1[0] * (-dir2[1]) - dir1[1] * (-dir2[0]);
    if det.abs() < f64::EPSILON {
        return None;
    }
    let rhs = [p2[0] - p1[0], p2[1] - p1[1]];
    Some((rhs[0] * (-dir2[1]) - rhs[1] * (-dir2[0])) / det)
}

/// Intersect the *infinite* line through `source` with `circle`. Returns
/// the `t` values along `(start, end)` where the line hits the circle,
/// sorted ascending.
fn parametric_intersections_with_circle(source: &Line, circle: &Circle) -> Vec<f64> {
    let dir = [
        source.end[0] - source.start[0],
        source.end[1] - source.start[1],
    ];
    let f = [
        source.start[0] - circle.center[0],
        source.start[1] - circle.center[1],
    ];
    let a = dot(dir, dir);
    let b = 2.0 * dot(f, dir);
    let c = dot(f, f) - circle.radius * circle.radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 || a.abs() < f64::EPSILON {
        return Vec::new();
    }
    let sd = disc.sqrt();
    let t1 = (-b - sd) / (2.0 * a);
    let t2 = (-b + sd) / (2.0 * a);
    let mut out = vec![t1, t2];
    out.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn point_at(line: &Line, t: f64) -> [f64; 2] {
    [
        line.start[0] + t * (line.end[0] - line.start[0]),
        line.start[1] + t * (line.end[1] - line.start[1]),
    ]
}

fn move_start_pref(source: &Line, pick: [f64; 2]) -> bool {
    let d_start = (source.start[0] - pick[0]).powi(2) + (source.start[1] - pick[1]).powi(2);
    let d_end = (source.end[0] - pick[0]).powi(2) + (source.end[1] - pick[1]).powi(2);
    d_start < d_end
}

impl ExtendTool {
    /// Extend `source` so the end closer to `pick_point` reaches the
    /// (infinite extension of) `boundary`.
    pub fn extend_to_line(source: &Line, boundary: &Line, pick_point: [f64; 2]) -> Option<Line> {
        let t = parametric_intersection_with_line(source, boundary)?;
        let move_start = move_start_pref(source, pick_point);
        // For a true *extend*, the new endpoint must lie outside the existing
        // segment in the appropriate direction.
        if move_start && t >= 0.0 {
            return None;
        }
        if !move_start && t <= 1.0 {
            return None;
        }
        let hit = point_at(source, t);
        Some(if move_start {
            Line {
                start: hit,
                ..source.clone()
            }
        } else {
            Line {
                end: hit,
                ..source.clone()
            }
        })
    }

    /// Extend `source` so the end closer to `pick_point` reaches the
    /// nearest intersection with `boundary`.
    pub fn extend_to_circle(
        source: &Line,
        boundary: &Circle,
        pick_point: [f64; 2],
    ) -> Option<Line> {
        let ts = parametric_intersections_with_circle(source, boundary);
        if ts.is_empty() {
            return None;
        }
        let move_start = move_start_pref(source, pick_point);
        // Pick the intersection in the extend direction that is *closest*
        // to the anchor end (i.e. the smaller required extension).
        let chosen = if move_start {
            // start moves outward in the negative-t direction → pick the
            // largest t < 0.
            ts.into_iter()
                .filter(|t| *t < 0.0)
                .fold(None::<f64>, |acc, t| match acc {
                    Some(best) if best > t => Some(best),
                    _ => Some(t),
                })
        } else {
            // end moves outward in t > 1 direction → pick the smallest t > 1.
            ts.into_iter()
                .filter(|t| *t > 1.0)
                .fold(None::<f64>, |acc, t| match acc {
                    Some(best) if best < t => Some(best),
                    _ => Some(t),
                })
        }?;
        let hit = point_at(source, chosen);
        Some(if move_start {
            Line {
                start: hit,
                ..source.clone()
            }
        } else {
            Line {
                end: hit,
                ..source.clone()
            }
        })
    }

    pub fn apply(
        source: &Primitive,
        boundary: &Primitive,
        pick_point: [f64; 2],
    ) -> Option<Primitive> {
        match (source, boundary) {
            (Primitive::Line(s), Primitive::Line(b)) => {
                Self::extend_to_line(s, b, pick_point).map(Primitive::Line)
            }
            (Primitive::Line(s), Primitive::Circle(c)) => {
                Self::extend_to_circle(s, c, pick_point).map(Primitive::Line)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_to_perpendicular_line() {
        // src on x-axis [0,0]-[2,0]; boundary x=5 vertical line.
        let src = Line::new("0", [0.0, 0.0], [2.0, 0.0]);
        let boundary = Line::new("0", [5.0, -10.0], [5.0, 10.0]);
        let ex = ExtendTool::extend_to_line(&src, &boundary, [2.0, 0.0]).unwrap();
        assert!((ex.end[0] - 5.0).abs() < 1e-9);
        assert!((ex.end[1] - 0.0).abs() < 1e-9);
        // Start unchanged.
        assert!((ex.start[0] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn extend_to_circle_picks_near_intersection() {
        let src = Line::new("0", [0.0, 0.0], [2.0, 0.0]);
        let c = Circle::new("0", [10.0, 0.0], 3.0);
        let ex = ExtendTool::extend_to_circle(&src, &c, [2.0, 0.0]).unwrap();
        // Near intersection is x=7.
        assert!((ex.end[0] - 7.0).abs() < 1e-9);
        assert!((ex.end[1] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn no_extend_if_already_intersects_within_segment() {
        let src = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let boundary = Line::new("0", [5.0, -10.0], [5.0, 10.0]);
        assert!(ExtendTool::extend_to_line(&src, &boundary, [10.0, 0.0]).is_none());
    }

    #[test]
    fn parallel_lines_return_none() {
        let src = Line::new("0", [0.0, 0.0], [2.0, 0.0]);
        let boundary = Line::new("0", [0.0, 5.0], [2.0, 5.0]);
        assert!(ExtendTool::extend_to_line(&src, &boundary, [2.0, 0.0]).is_none());
    }
}
