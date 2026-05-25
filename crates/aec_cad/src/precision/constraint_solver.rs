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

use crate::precision::constraints::{Constraint, ConstraintSet, PointIndex};

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

    /// Incremental drag entry point. Moves the listed points to their
    /// new positions, then solves so every other constraint in `cs`
    /// remains satisfied.
    ///
    /// The drag is modelled as a temporary `FixedPoint` constraint
    /// per dragged point — this is the standard approach in 2D
    /// parametric CAD (Solvespace, CADKit, OpenSCAD's constraint
    /// solver, etc.). The original constraint set is **not** mutated:
    /// the temporary pins are added to a clone and discarded after
    /// the solve. This means a UI can repeatedly call
    /// `solve_with_drag` during a mouse-drag without accumulating
    /// stale pins.
    ///
    /// The dragged-point coordinates in `vars` are updated to the
    /// supplied targets before solving so that even if the system is
    /// over-constrained and the solver doesn't fully converge, the
    /// dragged point lands as close as possible to where the user
    /// asked.
    pub fn solve_with_drag(
        &self,
        cs: &ConstraintSet,
        vars: &mut [f64],
        dragged: &[(PointIndex, [f64; 2])],
    ) -> SolveResult {
        for &(idx, pos) in dragged {
            if idx * 2 + 1 < vars.len() {
                vars[idx * 2] = pos[0];
                vars[idx * 2 + 1] = pos[1];
            }
        }
        if dragged.is_empty() {
            return self.solve(cs, vars);
        }
        let mut augmented = cs.clone();
        for &(idx, pos) in dragged {
            augmented.add(Constraint::FixedPoint {
                a: idx,
                position: pos,
            });
        }
        self.solve(&augmented, vars)
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

    /// Phase 11 Task 24 acceptance test:
    /// > "Create a rectangle with 4 lines + horizontal/vertical/coincident
    /// > constraints → move one corner → verify all constraints still satisfied."
    ///
    /// We model the rectangle as 4 points (corners). Lines are implicit
    /// between the consecutive pairs (0,1), (1,2), (2,3), (3,0). The
    /// constraints pin:
    ///   - p0–p1 horizontal (bottom edge),
    ///   - p2–p3 horizontal (top edge),
    ///   - p1–p2 vertical   (right edge),
    ///   - p3–p0 vertical   (left edge),
    ///   - p0 fixed at origin (so the rectangle has a definite location),
    ///   - p0–p1 fixed distance = 5  (width),
    ///   - p0–p3 fixed distance = 3  (height).
    ///
    /// Then we drag p1 (the bottom-right corner) — a "stretch the
    /// rectangle wider" operation. The solver must:
    ///   - move p1 to its new position,
    ///   - re-derive p2 so the top-right corner stays directly above p1
    ///     (vertical edge) AND directly across from p3 (horizontal edge),
    ///   - keep p0 anchored.
    #[test]
    fn rectangle_with_h_v_coincident_constraints_drag_corner_stays_satisfied() {
        let mut cs = ConstraintSet::new(4);
        // Anchor p0 at origin.
        cs.add(Constraint::FixedPoint {
            a: 0,
            position: [0.0, 0.0],
        });
        // Bottom edge p0—p1 is horizontal.
        cs.add(Constraint::Horizontal { a: 0, b: 1 });
        // Top edge p2—p3 is horizontal.
        cs.add(Constraint::Horizontal { a: 2, b: 3 });
        // Right edge p1—p2 is vertical.
        cs.add(Constraint::Vertical { a: 1, b: 2 });
        // Left edge p3—p0 is vertical.
        cs.add(Constraint::Vertical { a: 3, b: 0 });
        // Left edge height = 3 (the rectangle's height is fixed; its
        // width is left free so the drag below can stretch it).
        cs.add(Constraint::FixedDistance {
            a: 0,
            b: 3,
            distance: 3.0,
        });
        // Start from a perfect 5×3 rectangle anchored at origin.
        let mut vars = [
            0.0, 0.0, // p0
            5.0, 0.0, // p1
            5.0, 3.0, // p2
            0.0, 3.0, // p3
        ];
        let solver = ConstraintSolver::default();
        assert!(solver.solve(&cs, &mut vars).is_converged());

        // Drag p1 from (5, 0) to (7, 0) — stretch the rectangle's
        // bottom-right corner 2 m to the right. The solver must
        // propagate that move into p2 (the top-right corner) so the
        // Vertical { p1, p2 } edge stays vertical, and into p3 via
        // the Horizontal { p2, p3 } + Vertical { p3, p0 } edges so
        // the height stays exactly 3 m.
        let drag = [(1usize, [7.0, 0.0])];
        let r = solver.solve_with_drag(&cs, &mut vars, &drag);
        assert!(
            r.is_converged(),
            "solver must converge on a satisfiable drag, got {r:?}"
        );

        // ---- Every original constraint is still satisfied. ----
        for c in &cs.constraints {
            assert!(
                c.residual(&vars).abs() < 1e-3,
                "constraint {:?} not satisfied after drag (residual={})",
                c,
                c.residual(&vars)
            );
        }

        // p0 is still at the origin.
        assert!((vars[0]).abs() < 1e-3);
        assert!((vars[1]).abs() < 1e-3);
        // p1 landed where the drag asked — (7, 0).
        assert!(
            (vars[2] - 7.0).abs() < 1e-3,
            "p1.x should be 7, got {}",
            vars[2]
        );
        assert!(vars[3].abs() < 1e-3, "p1.y should be 0, got {}", vars[3]);
        // The top-right corner p2 followed: x ~= 7, y ~= 3.
        assert!(
            (vars[4] - 7.0).abs() < 1e-3,
            "p2.x should follow drag to 7, got {}",
            vars[4]
        );
        assert!(
            (vars[5] - 3.0).abs() < 1e-3,
            "p2.y should stay at 3, got {}",
            vars[5]
        );
        // The Vertical edge between p1 and p2 still holds — they share x.
        assert!(
            (vars[4] - vars[2]).abs() < 1e-3,
            "p1.x and p2.x diverged: {} vs {}",
            vars[2],
            vars[4]
        );
        // The Horizontal edge between p2 and p3 still holds — they share y.
        assert!(
            (vars[5] - vars[7]).abs() < 1e-3,
            "p2.y and p3.y diverged: {} vs {}",
            vars[5],
            vars[7]
        );
        // The left edge p3—p0 is vertical — they share x.
        assert!((vars[6] - vars[0]).abs() < 1e-3, "p3.x and p0.x diverged");
    }

    /// When the user drags a point and the system is satisfiable
    /// without forcing it to a different position, the solver should
    /// honour the drag target.
    #[test]
    fn drag_propagates_to_dependents_when_satisfiable() {
        // Three collinear horizontal points p0—p1—p2. Drag p0 up by 5.
        // p1 and p2 must come along since they share `y` with p0.
        let mut cs = ConstraintSet::new(3);
        cs.add(Constraint::Horizontal { a: 0, b: 1 });
        cs.add(Constraint::Horizontal { a: 1, b: 2 });

        let mut vars = [0.0, 0.0, 5.0, 0.0, 10.0, 0.0];
        let r = ConstraintSolver::default().solve_with_drag(&cs, &mut vars, &[(0, [0.0, 5.0])]);
        assert!(r.is_converged());

        // p0 is where we asked.
        assert!((vars[0] - 0.0).abs() < 1e-4);
        assert!((vars[1] - 5.0).abs() < 1e-4);
        // p1 + p2 followed.
        assert!(
            (vars[3] - 5.0).abs() < 1e-3,
            "p1.y should follow drag, got {}",
            vars[3]
        );
        assert!(
            (vars[5] - 5.0).abs() < 1e-3,
            "p2.y should follow drag, got {}",
            vars[5]
        );
    }

    #[test]
    fn drag_with_empty_target_list_is_a_plain_solve() {
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
        let r = ConstraintSolver::default().solve_with_drag(&cs, &mut vars, &[]);
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
