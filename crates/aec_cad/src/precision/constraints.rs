//! Parametric 2D constraints — stored on a per-drawing basis and solved
//! incrementally by the constraint solver after any edit that moves an
//! involved entity vertex.

use serde::{Deserialize, Serialize};

/// Index into the solver's coordinate vector: `vars[idx*2]` = x,
/// `vars[idx*2+1]` = y.
pub type PointIndex = usize;

/// A single 2D parametric constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    Horizontal {
        a: PointIndex,
        b: PointIndex,
    },
    Vertical {
        a: PointIndex,
        b: PointIndex,
    },
    Coincident {
        a: PointIndex,
        b: PointIndex,
    },
    Parallel {
        a: PointIndex,
        b: PointIndex,
        c: PointIndex,
        d: PointIndex,
    },
    Perpendicular {
        a: PointIndex,
        b: PointIndex,
        c: PointIndex,
        d: PointIndex,
    },
    Tangent {
        line_start: PointIndex,
        line_end: PointIndex,
        center: PointIndex,
        radius: f64,
    },
    Equal {
        a: PointIndex,
        b: PointIndex,
        c: PointIndex,
        d: PointIndex,
    },
    FixedDistance {
        a: PointIndex,
        b: PointIndex,
        distance: f64,
    },
    FixedAngle {
        a: PointIndex,
        b: PointIndex,
        angle_deg: f64,
    },
    FixedPoint {
        a: PointIndex,
        position: [f64; 2],
    },
}

impl Constraint {
    /// Residual: the signed error this constraint still has given the
    /// current variable vector. Zero = satisfied.
    pub fn residual(&self, vars: &[f64]) -> f64 {
        match *self {
            Constraint::Horizontal { a, b } => vars[b * 2 + 1] - vars[a * 2 + 1],
            Constraint::Vertical { a, b } => vars[b * 2] - vars[a * 2],
            Constraint::Coincident { a, b } => {
                let dx = vars[b * 2] - vars[a * 2];
                let dy = vars[b * 2 + 1] - vars[a * 2 + 1];
                (dx * dx + dy * dy).sqrt()
            }
            Constraint::FixedDistance { a, b, distance } => {
                let dx = vars[b * 2] - vars[a * 2];
                let dy = vars[b * 2 + 1] - vars[a * 2 + 1];
                (dx * dx + dy * dy).sqrt() - distance
            }
            Constraint::FixedAngle { a, b, angle_deg } => {
                let dx = vars[b * 2] - vars[a * 2];
                let dy = vars[b * 2 + 1] - vars[a * 2 + 1];
                let actual = dy.atan2(dx).to_degrees();
                let mut delta = actual - angle_deg;
                while delta > 180.0 {
                    delta -= 360.0;
                }
                while delta < -180.0 {
                    delta += 360.0;
                }
                delta
            }
            Constraint::FixedPoint { a, position } => {
                let dx = vars[a * 2] - position[0];
                let dy = vars[a * 2 + 1] - position[1];
                (dx * dx + dy * dy).sqrt()
            }
            Constraint::Parallel { a, b, c, d } => {
                let v1x = vars[b * 2] - vars[a * 2];
                let v1y = vars[b * 2 + 1] - vars[a * 2 + 1];
                let v2x = vars[d * 2] - vars[c * 2];
                let v2y = vars[d * 2 + 1] - vars[c * 2 + 1];
                v1x * v2y - v1y * v2x
            }
            Constraint::Perpendicular { a, b, c, d } => {
                let v1x = vars[b * 2] - vars[a * 2];
                let v1y = vars[b * 2 + 1] - vars[a * 2 + 1];
                let v2x = vars[d * 2] - vars[c * 2];
                let v2y = vars[d * 2 + 1] - vars[c * 2 + 1];
                v1x * v2x + v1y * v2y
            }
            Constraint::Tangent {
                line_start,
                line_end,
                center,
                radius,
            } => {
                let lsx = vars[line_start * 2];
                let lsy = vars[line_start * 2 + 1];
                let lex = vars[line_end * 2];
                let ley = vars[line_end * 2 + 1];
                let cx = vars[center * 2];
                let cy = vars[center * 2 + 1];
                let dx = lex - lsx;
                let dy = ley - lsy;
                let len2 = dx * dx + dy * dy;
                if len2 < f64::EPSILON {
                    return radius;
                }
                let t = ((cx - lsx) * dx + (cy - lsy) * dy) / len2;
                let fx = lsx + t * dx;
                let fy = lsy + t * dy;
                let dist = ((cx - fx).powi(2) + (cy - fy).powi(2)).sqrt();
                dist - radius
            }
            Constraint::Equal { a, b, c, d } => {
                let dx1 = vars[b * 2] - vars[a * 2];
                let dy1 = vars[b * 2 + 1] - vars[a * 2 + 1];
                let dx2 = vars[d * 2] - vars[c * 2];
                let dy2 = vars[d * 2 + 1] - vars[c * 2 + 1];
                (dx1 * dx1 + dy1 * dy1).sqrt() - (dx2 * dx2 + dy2 * dy2).sqrt()
            }
        }
    }
}

/// Summary status of a constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintStatus {
    Satisfied,
    Unsatisfied,
    Overconstrained,
}

/// A set of constraints on a set of 2D points.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConstraintSet {
    pub constraints: Vec<Constraint>,
    /// Number of 2D points (each occupies 2 variables in the solver vector).
    pub point_count: usize,
}

impl ConstraintSet {
    pub fn new(point_count: usize) -> Self {
        Self {
            constraints: Vec::new(),
            point_count,
        }
    }

    pub fn add(&mut self, c: Constraint) {
        self.constraints.push(c);
    }

    pub fn remove(&mut self, idx: usize) {
        if idx < self.constraints.len() {
            self.constraints.remove(idx);
        }
    }

    pub fn status(&self, vars: &[f64], tolerance: f64) -> ConstraintStatus {
        let dof = self.point_count * 2;
        if self.constraints.len() > dof {
            return ConstraintStatus::Overconstrained;
        }
        for c in &self.constraints {
            if c.residual(vars).abs() > tolerance {
                return ConstraintStatus::Unsatisfied;
            }
        }
        ConstraintStatus::Satisfied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_residual_zero_when_same_y() {
        let c = Constraint::Horizontal { a: 0, b: 1 };
        let vars = [0.0, 5.0, 10.0, 5.0];
        assert!((c.residual(&vars)).abs() < 1e-9);
    }

    #[test]
    fn fixed_distance_residual() {
        let c = Constraint::FixedDistance {
            a: 0,
            b: 1,
            distance: 5.0,
        };
        let vars = [0.0, 0.0, 3.0, 4.0]; // dist = 5
        assert!((c.residual(&vars)).abs() < 1e-9);
    }

    #[test]
    fn parallel_cross_product_zero() {
        let c = Constraint::Parallel {
            a: 0,
            b: 1,
            c: 2,
            d: 3,
        };
        let vars = [0.0, 0.0, 2.0, 1.0, 5.0, 0.0, 7.0, 1.0];
        assert!((c.residual(&vars)).abs() < 1e-9);
    }

    #[test]
    fn overconstrained_set() {
        let mut cs = ConstraintSet::new(1);
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        assert_eq!(
            cs.status(&[0.0, 0.0], 1e-6),
            ConstraintStatus::Overconstrained
        );
    }
}
