//! Trim tool — clip a line at its intersection with a cutting entity.
//!
//! The pick point selects which end of the source segment is removed.

use crate::primitives::{Circle, Line, Primitive};

pub struct TrimTool;

/// Line-line segment intersection. Returns the intersection point if the
/// two segments cross (parameters in [0,1]).
pub fn line_line_intersection(a: &Line, b: &Line) -> Option<[f64; 2]> {
    let p1 = a.start;
    let p2 = a.end;
    let p3 = b.start;
    let p4 = b.end;
    let d1x = p2[0] - p1[0];
    let d1y = p2[1] - p1[1];
    let d2x = p4[0] - p3[0];
    let d2y = p4[1] - p3[1];
    let denom = d1x * d2y - d1y * d2x;
    if denom.abs() < 1e-9 {
        return None;
    }
    let dx = p3[0] - p1[0];
    let dy = p3[1] - p1[1];
    let t = (dx * d2y - dy * d2x) / denom;
    let s = (dx * d1y - dy * d1x) / denom;
    if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&s) {
        return None;
    }
    Some([p1[0] + t * d1x, p1[1] + t * d1y])
}

/// Line-circle intersection points (0, 1, or 2 hits clamped to segment).
pub fn line_circle_intersections(line: &Line, circle: &Circle) -> Vec<[f64; 2]> {
    let dx = line.end[0] - line.start[0];
    let dy = line.end[1] - line.start[1];
    let fx = line.start[0] - circle.center[0];
    let fy = line.start[1] - circle.center[1];
    let a = dx * dx + dy * dy;
    let b = 2.0 * (fx * dx + fy * dy);
    let c = fx * fx + fy * fy - circle.radius * circle.radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 || a < f64::EPSILON {
        return Vec::new();
    }
    let sqrt_d = disc.sqrt();
    let t1 = (-b - sqrt_d) / (2.0 * a);
    let t2 = (-b + sqrt_d) / (2.0 * a);
    let mut out = Vec::new();
    for t in [t1, t2] {
        if (0.0..=1.0).contains(&t) {
            out.push([line.start[0] + t * dx, line.start[1] + t * dy]);
        }
    }
    out
}

impl TrimTool {
    /// Trim the source line at the cut's intersection. `pick_point` is
    /// the point of the source the user clicked on — the piece *containing*
    /// pick_point is removed.
    pub fn trim_line_at_line(source: &Line, cut: &Line, pick_point: [f64; 2]) -> Option<Line> {
        let hit = line_line_intersection(source, cut)?;
        let d_start = sq(source.start, pick_point);
        let d_end = sq(source.end, pick_point);
        Some(if d_start < d_end {
            // Pick is closer to start → remove start side, keep [hit, end].
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

    /// Trim the source line at its intersection with a circle. `pick_point`
    /// is the point of the source the user clicked on — the piece *containing*
    /// `pick_point` is removed, mirroring [`Self::trim_line_at_line`].
    ///
    /// When the line crosses the circle twice and `pick_point` lies between
    /// the two intersections, the inner segment is removed and the result is
    /// undefined (returns `None`) — `apply` callers should split into two
    /// separate trims for that case.
    pub fn trim_line_at_circle(source: &Line, cut: &Circle, pick_point: [f64; 2]) -> Option<Line> {
        let hits = line_circle_intersections(source, cut);
        if hits.is_empty() {
            return None;
        }
        let d_start = sq(source.start, pick_point);
        let d_end = sq(source.end, pick_point);
        // Pick the intersection nearest the picked endpoint — this is the
        // boundary between the removed piece (containing `pick_point`) and
        // the kept piece.
        if d_start < d_end {
            let hit = hits
                .into_iter()
                .min_by(|p, q| {
                    sq(*p, source.start)
                        .partial_cmp(&sq(*q, source.start))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            // Pick is closer to start → remove start side, keep [hit, end].
            Some(Line {
                start: hit,
                ..source.clone()
            })
        } else {
            let hit = hits
                .into_iter()
                .min_by(|p, q| {
                    sq(*p, source.end)
                        .partial_cmp(&sq(*q, source.end))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            // Pick is closer to end → remove end side, keep [start, hit].
            Some(Line {
                end: hit,
                ..source.clone()
            })
        }
    }

    pub fn apply(source: &Primitive, cut: &Primitive, pick_point: [f64; 2]) -> Option<Primitive> {
        match (source, cut) {
            (Primitive::Line(s), Primitive::Line(c)) => {
                Self::trim_line_at_line(s, c, pick_point).map(Primitive::Line)
            }
            (Primitive::Line(s), Primitive::Circle(c)) => {
                Self::trim_line_at_circle(s, c, pick_point).map(Primitive::Line)
            }
            _ => None,
        }
    }
}

fn sq(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_line_intersect_basic() {
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [5.0, -5.0], [5.0, 5.0]);
        let p = line_line_intersection(&a, &b).unwrap();
        assert!((p[0] - 5.0).abs() < 1e-9);
        assert!((p[1] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn parallel_lines_no_intersection() {
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [0.0, 1.0], [10.0, 1.0]);
        assert!(line_line_intersection(&a, &b).is_none());
    }

    #[test]
    fn trim_keeps_right_portion_when_pick_left() {
        let src = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let cut = Line::new("0", [5.0, -1.0], [5.0, 1.0]);
        let trimmed = TrimTool::trim_line_at_line(&src, &cut, [1.0, 0.0]).unwrap();
        assert!((trimmed.start[0] - 5.0).abs() < 1e-9);
        assert!((trimmed.end[0] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn line_circle_two_intersections() {
        let l = Line::new("0", [-10.0, 0.0], [10.0, 0.0]);
        let c = Circle::new("0", [0.0, 0.0], 5.0);
        let hits = line_circle_intersections(&l, &c);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn trim_line_at_circle_pick_near_start_removes_start_side() {
        // Line from (-10,0) to (10,0), circle radius 5 at origin → hits at ±5.
        // Pick at (-9,0) → start side contains pick → remove start side, keep [-5, 10].
        let src = Line::new("0", [-10.0, 0.0], [10.0, 0.0]);
        let cut = Circle::new("0", [0.0, 0.0], 5.0);
        let trimmed = TrimTool::trim_line_at_circle(&src, &cut, [-9.0, 0.0]).unwrap();
        assert!((trimmed.start[0] - (-5.0)).abs() < 1e-9);
        assert!((trimmed.end[0] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn trim_line_at_circle_pick_near_end_removes_end_side() {
        // Same line/circle but pick at (9,0) → end side contains pick → keep [-10, 5].
        let src = Line::new("0", [-10.0, 0.0], [10.0, 0.0]);
        let cut = Circle::new("0", [0.0, 0.0], 5.0);
        let trimmed = TrimTool::trim_line_at_circle(&src, &cut, [9.0, 0.0]).unwrap();
        assert!((trimmed.start[0] - (-10.0)).abs() < 1e-9);
        assert!((trimmed.end[0] - 5.0).abs() < 1e-9);
    }
}
