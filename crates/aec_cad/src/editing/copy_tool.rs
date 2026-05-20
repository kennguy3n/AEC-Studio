//! Copy tool — translate clones of the source primitives.
//!
//! Supports single-step copies and rectangular / polar arrays.

use crate::primitives::{Affine2, Primitive};

pub struct CopyTool;

impl CopyTool {
    /// One clone translated by `delta`.
    pub fn apply(primitive: &Primitive, delta: [f64; 2]) -> Primitive {
        primitive.transformed(&Affine2::translation(delta[0], delta[1]))
    }

    /// Rectangular array: `rows × cols`, with `row_step` and `col_step`
    /// vectors. The output includes the original at index (0,0).
    pub fn rectangular_array(
        primitive: &Primitive,
        rows: usize,
        cols: usize,
        row_step: [f64; 2],
        col_step: [f64; 2],
    ) -> Vec<Primitive> {
        let mut out = Vec::with_capacity(rows.max(1) * cols.max(1));
        for r in 0..rows.max(1) {
            for c in 0..cols.max(1) {
                let dx = c as f64 * col_step[0] + r as f64 * row_step[0];
                let dy = c as f64 * col_step[1] + r as f64 * row_step[1];
                out.push(primitive.transformed(&Affine2::translation(dx, dy)));
            }
        }
        out
    }

    /// Polar array: `count` copies rotated about `center` covering
    /// `total_angle_deg` total. Includes the original at angle 0.
    pub fn polar_array(
        primitive: &Primitive,
        center: [f64; 2],
        count: usize,
        total_angle_deg: f64,
    ) -> Vec<Primitive> {
        let mut out = Vec::with_capacity(count.max(1));
        let count = count.max(1);
        let step = if count > 1 {
            total_angle_deg / (count - 1) as f64
        } else {
            0.0
        };
        for i in 0..count {
            let a = step * i as f64;
            out.push(primitive.transformed(&Affine2::rotation_about(center, a)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{Circle, Line};

    #[test]
    fn rectangular_array_count() {
        let l = Primitive::Line(Line::new("0", [0.0, 0.0], [1.0, 0.0]));
        let arr = CopyTool::rectangular_array(&l, 3, 4, [0.0, 2.0], [2.0, 0.0]);
        assert_eq!(arr.len(), 12);
    }

    #[test]
    fn polar_array_three_quarter_turn() {
        let c = Primitive::Circle(Circle::new("0", [1.0, 0.0], 0.1));
        let arr = CopyTool::polar_array(&c, [0.0, 0.0], 4, 270.0);
        assert_eq!(arr.len(), 4);
        if let Primitive::Circle(c4) = &arr[3] {
            assert!((c4.center[0] - 0.0).abs() < 1e-6);
            assert!((c4.center[1] + 1.0).abs() < 1e-6);
        } else {
            unreachable!()
        }
    }
}
