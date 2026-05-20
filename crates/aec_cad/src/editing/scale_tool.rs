//! Scale tool — uniform or non-uniform scale about a pivot.

use crate::primitives::{Affine2, Primitive};

pub struct ScaleTool;

impl ScaleTool {
    pub fn apply(primitive: &Primitive, pivot: [f64; 2], factor: f64) -> Primitive {
        primitive.transformed(&Affine2::scaling_about(pivot, factor, factor))
    }

    pub fn apply_nonuniform(primitive: &Primitive, pivot: [f64; 2], sx: f64, sy: f64) -> Primitive {
        primitive.transformed(&Affine2::scaling_about(pivot, sx, sy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Circle;

    #[test]
    fn double_circle_doubles_radius() {
        let c = Primitive::Circle(Circle::new("0", [0.0, 0.0], 5.0));
        let s = ScaleTool::apply(&c, [0.0, 0.0], 2.0);
        if let Primitive::Circle(c2) = s {
            assert!((c2.radius - 10.0).abs() < 1e-9);
        }
    }
}
