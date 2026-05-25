//! Phase 11 Task 24 — incremental constraint solver e2e.
//!
//! Builds a small library of parametric sketches and verifies that
//! dragging individual points triggers correct propagation through
//! every supported constraint kind (horizontal, vertical, coincident,
//! parallel, perpendicular, equal, tangent, fixed-point, fixed-
//! distance, fixed-angle).

use aec_cad::precision::constraint_solver::{ConstraintSolver, SolveResult};
use aec_cad::precision::constraints::{Constraint, ConstraintSet, ConstraintStatus};

fn solver() -> ConstraintSolver {
    ConstraintSolver {
        max_iterations: 200,
        tolerance: 1e-6,
        damping: 1.0,
    }
}

fn assert_converged(r: &SolveResult) {
    assert!(
        matches!(r, SolveResult::Converged { .. }),
        "expected Converged, got {r:?}"
    );
}

#[test]
fn rectangle_drag_stretches_keeping_all_edges_orthogonal() {
    // p0 = bottom-left, p1 = bottom-right, p2 = top-right, p3 = top-left.
    let mut cs = ConstraintSet::new(4);
    cs.add(Constraint::FixedPoint {
        a: 0,
        position: [0.0, 0.0],
    });
    cs.add(Constraint::Horizontal { a: 0, b: 1 });
    cs.add(Constraint::Horizontal { a: 2, b: 3 });
    cs.add(Constraint::Vertical { a: 1, b: 2 });
    cs.add(Constraint::Vertical { a: 3, b: 0 });
    cs.add(Constraint::FixedDistance {
        a: 0,
        b: 3,
        distance: 4.0, // height
    });

    let mut vars = [0.0, 0.0, 6.0, 0.0, 6.0, 4.0, 0.0, 4.0];
    assert_converged(&solver().solve(&cs, &mut vars));

    // Drag bottom-right corner +3 m to the right and -1 m down (the
    // down component must be cancelled because Horizontal{p0,p1}
    // pulls p1's y to 0, and p0 is anchored).
    // Solver target: (9, 0).
    assert_converged(&solver().solve_with_drag(&cs, &mut vars, &[(1usize, [9.0, 0.0])]));

    assert_eq!(cs.status(&vars, 1e-3), ConstraintStatus::Satisfied);
    // Rectangle is now 9 × 4.
    assert!((vars[2] - 9.0).abs() < 1e-3);
    assert!((vars[4] - 9.0).abs() < 1e-3); // p2.x == p1.x
    assert!((vars[5] - 4.0).abs() < 1e-3); // p2.y == 4
    assert!((vars[7] - 4.0).abs() < 1e-3); // p3.y == 4
    assert!((vars[6] - 0.0).abs() < 1e-3); // p3.x == 0
}

#[test]
fn parallelogram_with_parallel_constraint_propagates_correctly() {
    // 4 points forming a parallelogram. p0 fixed at origin. p0->p1
    // and p3->p2 are parallel. p0->p3 and p1->p2 are parallel.
    let mut cs = ConstraintSet::new(4);
    cs.add(Constraint::FixedPoint {
        a: 0,
        position: [0.0, 0.0],
    });
    cs.add(Constraint::Parallel {
        a: 0,
        b: 1,
        c: 3,
        d: 2,
    });
    cs.add(Constraint::Parallel {
        a: 0,
        b: 3,
        c: 1,
        d: 2,
    });

    let mut vars = [0.0, 0.0, 4.0, 0.0, 5.0, 3.0, 1.0, 3.0];
    assert_converged(&solver().solve(&cs, &mut vars));

    // Drag p1 to (5, 0.5). Both parallel constraints must still hold.
    assert_converged(&solver().solve_with_drag(&cs, &mut vars, &[(1usize, [5.0, 0.5])]));

    for c in &cs.constraints {
        assert!(
            c.residual(&vars).abs() < 1e-2,
            "{c:?} unsatisfied after drag, residual = {}",
            c.residual(&vars)
        );
    }
}

#[test]
fn equal_length_constraint_keeps_two_edges_in_sync() {
    // Two edges (p0–p1) and (p2–p3). Equal length constraint between
    // them. Anchor one edge fixed. Drag the other's endpoint and
    // verify the length matches.
    let mut cs = ConstraintSet::new(4);
    cs.add(Constraint::FixedPoint {
        a: 0,
        position: [0.0, 0.0],
    });
    cs.add(Constraint::FixedPoint {
        a: 1,
        position: [3.0, 4.0],
    }); // edge length = 5
    cs.add(Constraint::FixedPoint {
        a: 2,
        position: [10.0, 0.0],
    });
    cs.add(Constraint::Equal {
        a: 0,
        b: 1,
        c: 2,
        d: 3,
    });

    let mut vars = [0.0, 0.0, 3.0, 4.0, 10.0, 0.0, 12.0, 0.0];
    // Initial state: edge 1 length = 5, edge 2 length = 2. Solve
    // should pull p3 outward so |p2–p3| = 5.
    assert_converged(&solver().solve(&cs, &mut vars));
    let edge_2 = ((vars[6] - vars[4]).powi(2) + (vars[7] - vars[5]).powi(2)).sqrt();
    assert!(
        (edge_2 - 5.0).abs() < 1e-3,
        "edge 2 should be 5, got {edge_2}"
    );
}

#[test]
fn perpendicular_constraint_keeps_lines_at_right_angle_after_drag() {
    // Line 1: p0–p1 (anchored along the x-axis).
    // Line 2: p2–p3 — only p2's *position* is anchored to land on
    // Line 1; p3 is free, and the Perpendicular constraint forces
    // Line 2 to leave Line 1 at a right angle.
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
        position: [2.5, 0.0],
    });
    cs.add(Constraint::Perpendicular {
        a: 0,
        b: 1,
        c: 2,
        d: 3,
    });

    let mut vars = [0.0, 0.0, 5.0, 0.0, 2.5, 0.0, 3.0, 2.0];
    assert_converged(&solver().solve(&cs, &mut vars));
    // Solving alone should already pull p3.x back to 2.5 (because
    // p0–p1 lies along the x-axis, perpendicular foot demands
    // p3.x == p2.x = 2.5).
    assert!(
        (vars[6] - 2.5).abs() < 1e-2,
        "after plain solve, p3.x should be 2.5, got {}",
        vars[6]
    );
    // Drag p3 *along* the perpendicular line to (2.5, 5). This is
    // a satisfiable target: only y changes.
    assert_converged(&solver().solve_with_drag(&cs, &mut vars, &[(3usize, [2.5, 5.0])]));
    let perp = &cs.constraints[3];
    assert!(
        perp.residual(&vars).abs() < 1e-3,
        "Perpendicular residual after legal drag = {}",
        perp.residual(&vars)
    );
    assert!((vars[6] - 2.5).abs() < 1e-3);
    assert!((vars[7] - 5.0).abs() < 1e-3);
}

#[test]
fn coincident_constraint_collapses_two_points() {
    // Two points that should overlap.
    let mut cs = ConstraintSet::new(2);
    cs.add(Constraint::Coincident { a: 0, b: 1 });

    let mut vars = [0.0, 0.0, 3.0, 4.0];
    assert_converged(&solver().solve(&cs, &mut vars));
    assert!((vars[0] - vars[2]).abs() < 1e-3);
    assert!((vars[1] - vars[3]).abs() < 1e-3);
}

#[test]
fn tangent_constraint_pulls_line_to_circle() {
    // Line p0–p1 should be tangent to a circle of radius 2 at p2.
    let mut cs = ConstraintSet::new(3);
    cs.add(Constraint::FixedPoint {
        a: 2,
        position: [0.0, 0.0],
    });
    cs.add(Constraint::FixedPoint {
        a: 0,
        position: [-5.0, 3.0],
    });
    cs.add(Constraint::Tangent {
        line_start: 0,
        line_end: 1,
        center: 2,
        radius: 2.0,
    });

    let mut vars = [-5.0, 3.0, 5.0, 3.5, 0.0, 0.0];
    assert_converged(&solver().solve(&cs, &mut vars));

    // Distance from origin to the resulting line == 2.
    let lsx = vars[0];
    let lsy = vars[1];
    let lex = vars[2];
    let ley = vars[3];
    let dx = lex - lsx;
    let dy = ley - lsy;
    let len2 = dx * dx + dy * dy;
    let t = (-lsx * dx + -lsy * dy) / len2;
    let fx = lsx + t * dx;
    let fy = lsy + t * dy;
    let d = (fx * fx + fy * fy).sqrt();
    assert!(
        (d - 2.0).abs() < 1e-2,
        "perpendicular distance should be 2, got {d}"
    );
}

#[test]
fn fixed_angle_constraint_rotates_segment() {
    // Vector p0→p1 at 30 degrees from x-axis.
    let mut cs = ConstraintSet::new(2);
    cs.add(Constraint::FixedPoint {
        a: 0,
        position: [0.0, 0.0],
    });
    cs.add(Constraint::FixedDistance {
        a: 0,
        b: 1,
        distance: 10.0,
    });
    cs.add(Constraint::FixedAngle {
        a: 0,
        b: 1,
        angle_deg: 30.0,
    });

    let mut vars = [0.0, 0.0, 9.0, 1.0];
    assert_converged(&solver().solve(&cs, &mut vars));
    // p1 should be at (10 cos 30°, 10 sin 30°) ≈ (8.66, 5.00).
    assert!((vars[2] - 10.0 * 30f64.to_radians().cos()).abs() < 1e-3);
    assert!((vars[3] - 10.0 * 30f64.to_radians().sin()).abs() < 1e-3);
}

#[test]
fn multi_point_drag_pins_each_dragged_point() {
    // Two points free. Drag both at once.
    let cs = ConstraintSet::new(2);
    let mut vars = [0.0, 0.0, 5.0, 5.0];
    let r = solver().solve_with_drag(&cs, &mut vars, &[(0, [1.0, 2.0]), (1, [-3.0, -4.0])]);
    assert_converged(&r);
    assert!((vars[0] - 1.0).abs() < 1e-6);
    assert!((vars[1] - 2.0).abs() < 1e-6);
    assert!((vars[2] - (-3.0)).abs() < 1e-6);
    assert!((vars[3] - (-4.0)).abs() < 1e-6);
}

#[test]
fn solve_does_not_mutate_constraint_set_on_drag() {
    let mut cs = ConstraintSet::new(2);
    cs.add(Constraint::Horizontal { a: 0, b: 1 });
    let original_len = cs.constraints.len();

    let mut vars = [0.0, 0.0, 5.0, 3.0];
    let _ = solver().solve_with_drag(&cs, &mut vars, &[(0, [1.0, 1.0])]);

    // The original constraint set should still have exactly 1 constraint;
    // the temporary FixedPoint pin should not have leaked into it.
    assert_eq!(cs.constraints.len(), original_len);
}

#[test]
fn repeated_drag_simulates_mouse_drag_trail() {
    // Simulate a continuous drag of p1 along a horizontal line. Each
    // intermediate solve should leave the system valid for the next
    // step. The Horizontal { p0, p1 } constraint keeps both points
    // at the same y; the drag pin on p1 picks the actual y value.
    let mut cs = ConstraintSet::new(2);
    cs.add(Constraint::Horizontal { a: 0, b: 1 });

    let mut vars = [0.0, 0.0, 5.0, 0.0];
    for step in 1..=10 {
        let target_x = 5.0 + step as f64 * 0.5;
        // Mouse pointer wobbles in y; with Horizontal, p0 follows.
        let target_y = (step as f64) * 0.01;
        let r = solver().solve_with_drag(&cs, &mut vars, &[(1, [target_x, target_y])]);
        assert_converged(&r);
        // p1 ended up where the drag asked.
        assert!((vars[2] - target_x).abs() < 1e-3);
        assert!((vars[3] - target_y).abs() < 1e-3);
        // p0.y = p1.y by Horizontal.
        assert!((vars[1] - vars[3]).abs() < 1e-3);
    }
}
