//! Move tool — translate a primitive by `delta`.

use crate::primitives::{Affine2, Primitive};

pub struct MoveTool;

impl MoveTool {
    pub fn apply(primitive: &Primitive, delta: [f64; 2]) -> Primitive {
        primitive.transformed(&Affine2::translation(delta[0], delta[1]))
    }

    pub fn apply_many(primitives: &[Primitive], delta: [f64; 2]) -> Vec<Primitive> {
        primitives.iter().map(|p| Self::apply(p, delta)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Line;

    #[test]
    fn move_line_translates_endpoints() {
        let l = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        let m = MoveTool::apply(&l, [5.0, 2.0]);
        if let Primitive::Line(line) = m {
            assert_eq!(line.start, [5.0, 2.0]);
            assert_eq!(line.end, [15.0, 2.0]);
        } else {
            unreachable!();
        }
    }
}
