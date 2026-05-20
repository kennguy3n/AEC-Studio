//! Newton-Raphson incremental constraint solver.
//!
//! Given a [`ConstraintSet`] and an initial variable vector (point
//! coordinates packed as `[x0, y0, x1, y1, …]`), the solver iterates
//! toward a state where every constraint's residual is within `tolerance`.
//!
//! We use finite-difference Jacobian approximation (central differences)
//! because it keeps the solver generic over all constraint types without
//! manual derivative expressions for each one. Performance is adequate
//! for the small systems (< 200 points) that arise in the CAD command-
//! line editor's constraint mode.

use crate::precision::constraints::ConstraintSet;

/// Solver result.
#[derive(Debug, Clone, PartialEq)]
pub enum SolveResult {
    Converged {
        iterations: usize,
        max_residual: f64,
    },
    Diverged {
        iterations: usize,
        max_residual: f64,
    },
}

impl SolveResult {
    pub fn is_converged(&self) -> bool {
        matches!(self, SolveResult::Converged { .. })
    }
}

pub struct ConstraintSolver {
    pub max_iterations: usize,
    pub tolerance: f64,
    pub damping: f64,
}

impl Default for ConstraintSolver {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            tolerance: 1e-6,
            damping: 1.0,
        }
    }
}

impl ConstraintSolver {
    /// Solve in-place. Returns the result status.
    pub fn solve(&self, cs: &ConstraintSet, vars: &mut [f64]) -> SolveResult {
        let m = cs.constraints.len();
        let n = vars.len();
        if m == 0 {
            return SolveResult::Converged {
                iterations: 0,
                max_residual: 0.0,
            };
        }
        for iteration in 0..self.max_iterations {
            let residuals: Vec<f64> = cs.constraints.iter().map(|c| c.residual(vars)).collect();
            let max_r = residuals.iter().map(|r| r.abs()).fold(0.0f64, f64::max);
            if max_r < self.tolerance {
                return SolveResult::Converged {
                    iterations: iteration,
                    max_residual: max_r,
                };
            }
            // Build Jacobian via central differences.
            let h = 1e-7;
            let mut jac = vec![0.0; m * n];
            for j in 0..n {
                let orig = vars[j];
                vars[j] = orig + h;
                let r_plus: Vec<f64> = cs.constraints.iter().map(|c| c.residual(vars)).collect();
                vars[j] = orig - h;
                let r_minus: Vec<f64> = cs.constraints.iter().map(|c| c.residual(vars)).collect();
                vars[j] = orig;
                for i in 0..m {
                    jac[i * n + j] = (r_plus[i] - r_minus[i]) / (2.0 * h);
                }
            }
            // Solve the normal equations: J^T J δ = J^T r (Gauss-Newton step).
            let delta = gauss_newton_step(&jac, &residuals, m, n);
            // Apply damped step.
            for j in 0..n {
                vars[j] -= self.damping * delta[j];
            }
        }
        let max_r = cs
            .constraints
            .iter()
            .map(|c| c.residual(vars).abs())
            .fold(0.0f64, f64::max);
        SolveResult::Diverged {
            iterations: self.max_iterations,
            max_residual: max_r,
        }
    }
}

/// Solve J^T J δ = J^T r via direct Cholesky-like factorisation (small n).
fn gauss_newton_step(jac: &[f64], residuals: &[f64], m: usize, n: usize) -> Vec<f64> {
    // A = J^T J  (n×n)
    let mut a = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            let mut sum = 0.0;
            for k in 0..m {
                sum += jac[k * n + i] * jac[k * n + j];
            }
            a[i * n + j] = sum;
        }
    }
    // b = J^T r  (n)
    let mut b = vec![0.0; n];
    for i in 0..n {
        let mut sum = 0.0;
        for k in 0..m {
            sum += jac[k * n + i] * residuals[k];
        }
        b[i] = sum;
    }
    // Regularise diagonal (Levenberg-Marquardt-like).
    let lam = 1e-10;
    for i in 0..n {
        a[i * n + i] += lam;
    }
    // Solve A δ = b via Gaussian elimination with partial pivoting.
    solve_dense(&mut a, &mut b, n);
    b
}

fn solve_dense(a: &mut [f64], b: &mut [f64], n: usize) {
    for col in 0..n {
        // Pivot.
        let mut max_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                max_row = row;
            }
        }
        if max_row != col {
            for j in 0..n {
                a.swap(col * n + j, max_row * n + j);
            }
            b.swap(col, max_row);
        }
        let diag = a[col * n + col];
        if diag.abs() < 1e-15 {
            continue;
        }
        for row in (col + 1)..n {
            let factor = a[row * n + col] / diag;
            for j in col..n {
                let v = a[col * n + j];
                a[row * n + j] -= factor * v;
            }
            let bc = b[col];
            b[row] -= factor * bc;
        }
    }
    // Back-substitution.
    for col in (0..n).rev() {
        let diag = a[col * n + col];
        if diag.abs() < 1e-15 {
            b[col] = 0.0;
            continue;
        }
        for j in (col + 1)..n {
            let v = a[col * n + j];
            b[col] -= v * b[j];
        }
        b[col] /= diag;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precision::constraints::Constraint;

    #[test]
    fn fix_one_point_to_origin() {
        let mut cs = ConstraintSet::new(1);
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        let mut vars = [3.0, 4.0];
        let r = ConstraintSolver::default().solve(&cs, &mut vars);
        assert!(r.is_converged());
        assert!((vars[0]).abs() < 1e-4);
        assert!((vars[1]).abs() < 1e-4);
    }

    #[test]
    fn horizontal_constraint_aligns_y() {
        let mut cs = ConstraintSet::new(2);
        cs.add(Constraint::Horizontal { a: 0, b: 1 });
        let mut vars = [0.0, 0.0, 10.0, 3.0]; // second point y=3, should → y≈0
        let r = ConstraintSolver::default().solve(&cs, &mut vars);
        assert!(r.is_converged());
        assert!((vars[3] - vars[1]).abs() < 1e-4);
    }

    #[test]
    fn fixed_distance_converges() {
        let mut cs = ConstraintSet::new(2);
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        cs.add(Constraint::FixedDistance {
            a: 0,
            b: 1,
            distance: 5.0,
        });
        let mut vars = [0.0, 0.0, 7.0, 0.0];
        let r = ConstraintSolver::default().solve(&cs, &mut vars);
        assert!(r.is_converged());
        let d = ((vars[2]).powi(2) + (vars[3]).powi(2)).sqrt();
        assert!((d - 5.0).abs() < 1e-4);
    }

    #[test]
    fn perpendicular_constraint() {
        let mut cs = ConstraintSet::new(4);
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        cs.add(Constraint::FixedPoint {
            a: 1,
            position: [5.0, 0.0],
        });
        cs.add(Constraint::FixedPoint {
            a: 2,
            position: [0.0, 0.0],
        });
        cs.add(Constraint::Perpendicular {
            a: 0,
            b: 1,
            c: 2,
            d: 3,
        });
        let mut vars = [0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0, 2.0];
        let r = ConstraintSolver::default().solve(&cs, &mut vars);
        assert!(r.is_converged());
        let dot =
            (vars[2] - vars[0]) * (vars[6] - vars[4]) + (vars[3] - vars[1]) * (vars[7] - vars[5]);
        assert!(dot.abs() < 1e-3);
    }
}
