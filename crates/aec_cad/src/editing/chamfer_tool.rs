//! Chamfer tool — replace the meeting corner of two lines with a
//! straight bevel of configurable setbacks.

use crate::primitives::Line;

pub struct ChamferTool;

pub struct ChamferResult {
    pub line_a: Line,
    pub line_b: Line,
    pub bevel: Line,
}

fn unit_from_corner(line: &Line, corner: [f64; 2]) -> [f64; 2] {
    let away =
        if (line.start[0] - corner[0]).abs() < 1e-9 && (line.start[1] - corner[1]).abs() < 1e-9 {
            line.end
        } else {
            line.start
        };
    let dx = away[0] - corner[0];
    let dy = away[1] - corner[1];
    let l = (dx * dx + dy * dy).sqrt();
    if l < f64::EPSILON {
        [0.0, 0.0]
    } else {
        [dx / l, dy / l]
    }
}

fn clip_to(line: &Line, corner: [f64; 2], new_point: [f64; 2]) -> Line {
    let mut new = line.clone();
    let d_start = (line.start[0] - corner[0]).powi(2) + (line.start[1] - corner[1]).powi(2);
    let d_end = (line.end[0] - corner[0]).powi(2) + (line.end[1] - corner[1]).powi(2);
    if d_start < d_end {
        new.start = new_point;
    } else {
        new.end = new_point;
    }
    new
}

impl ChamferTool {
    pub fn chamfer_equal(
        a: &Line,
        b: &Line,
        corner: [f64; 2],
        setback: f64,
    ) -> Option<ChamferResult> {
        Self::chamfer_lines(a, b, corner, setback, setback)
    }

    pub fn chamfer_lines(
        a: &Line,
        b: &Line,
        corner: [f64; 2],
        setback_a: f64,
        setback_b: f64,
    ) -> Option<ChamferResult> {
        if setback_a < 0.0 || setback_b < 0.0 {
            return None;
        }
        let da = unit_from_corner(a, corner);
        let db = unit_from_corner(b, corner);
        let pa = [corner[0] + da[0] * setback_a, corner[1] + da[1] * setback_a];
        let pb = [corner[0] + db[0] * setback_b, corner[1] + db[1] * setback_b];
        Some(ChamferResult {
            line_a: clip_to(a, corner, pa),
            line_b: clip_to(b, corner, pb),
            bevel: Line::new(a.layer.clone(), pa, pb),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_chamfer_at_right_angle() {
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [0.0, 0.0], [0.0, 10.0]);
        let r = ChamferTool::chamfer_equal(&a, &b, [0.0, 0.0], 2.0).unwrap();
        assert_eq!(r.bevel.start, [2.0, 0.0]);
        assert_eq!(r.bevel.end, [0.0, 2.0]);
        assert_eq!(r.line_a.start, [2.0, 0.0]);
        assert_eq!(r.line_b.start, [0.0, 2.0]);
    }

    #[test]
    fn unequal_chamfer_setbacks() {
        let a = Line::new("0", [0.0, 0.0], [10.0, 0.0]);
        let b = Line::new("0", [0.0, 0.0], [0.0, 10.0]);
        let r = ChamferTool::chamfer_lines(&a, &b, [0.0, 0.0], 3.0, 5.0).unwrap();
        assert_eq!(r.line_a.start, [3.0, 0.0]);
        assert_eq!(r.line_b.start, [0.0, 5.0]);
    }
}
