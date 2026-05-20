//! Rotate tool — rotate a primitive about a pivot point.

use crate::primitives::{Affine2, Primitive};

pub struct RotateTool;

impl RotateTool {
    pub fn apply(primitive: &Primitive, pivot: [f64; 2], angle_deg: f64) -> Primitive {
        primitive.transformed(&Affine2::rotation_about(pivot, angle_deg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Line;

    #[test]
    fn ninety_degrees_rotates_x_to_y() {
        let l = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        let r = RotateTool::apply(&l, [0.0, 0.0], 90.0);
        if let Primitive::Line(line) = r {
            assert!((line.end[0] - 0.0).abs() < 1e-9);
            assert!((line.end[1] - 10.0).abs() < 1e-9);
        }
    }
}
