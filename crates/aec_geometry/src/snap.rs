//! Snap engine for the Design viewport.
//!
//! Supported targets: endpoint, midpoint, intersection, perpendicular, grid.
//! All snap calculations work in 2D plan-view coordinates (mm) — 3D-aware
//! snapping (e.g. face snap) is a Phase 3 follow-up.

use glam::DVec2;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapTarget {
    Endpoint,
    Midpoint,
    Intersection,
    Perpendicular,
    Grid,
}

impl SnapTarget {
    /// CAD precedence: lower number wins on near-ties. Matches the
    /// AutoCAD-style ordering preferred by AEC users.
    pub fn priority(self) -> u8 {
        match self {
            Self::Endpoint => 0,
            Self::Intersection => 1,
            Self::Midpoint => 2,
            Self::Perpendicular => 3,
            Self::Grid => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapResult {
    pub point_mm: [f64; 2],
    pub target: SnapTarget,
    pub distance_mm: f64,
}

/// Snap `query_mm` to the nearest snap candidate from a set of line segments
/// and a grid. Returns `None` when nothing is within `tolerance_mm`.
pub fn snap_to(
    query_mm: [f64; 2],
    segments: &[([f64; 2], [f64; 2])],
    grid_mm: f64,
    tolerance_mm: f64,
) -> Option<SnapResult> {
    let q = DVec2::new(query_mm[0], query_mm[1]);
    let mut best: Option<SnapResult> = None;

    // Tolerance for treating two distances as a tie when priority breaks ties.
    let tie_tolerance = 5.0_f64.min(tolerance_mm * 0.5).max(0.5);
    let mut candidate = |point: DVec2, target: SnapTarget| {
        let d = (point - q).length();
        if d <= tolerance_mm {
            let candidate = SnapResult { point_mm: [point.x, point.y], target, distance_mm: d };
            best = Some(match &best {
                Some(b) => {
                    let diff = (b.distance_mm - d).abs();
                    if diff <= tie_tolerance {
                        if target.priority() < b.target.priority() {
                            candidate
                        } else {
                            b.clone()
                        }
                    } else if d < b.distance_mm {
                        candidate
                    } else {
                        b.clone()
                    }
                }
                None => candidate,
            });
        }
    };

    for (a, b) in segments {
        let pa = DVec2::new(a[0], a[1]);
        let pb = DVec2::new(b[0], b[1]);
        // Endpoint.
        candidate(pa, SnapTarget::Endpoint);
        candidate(pb, SnapTarget::Endpoint);
        // Midpoint.
        candidate((pa + pb) * 0.5, SnapTarget::Midpoint);
        // Perpendicular foot.
        let ab = pb - pa;
        let len2 = ab.length_squared();
        if len2 > f64::EPSILON {
            let t = ((q - pa).dot(ab) / len2).clamp(0.0, 1.0);
            candidate(pa + ab * t, SnapTarget::Perpendicular);
        }
    }

    // Intersections between every pair of segments.
    for i in 0..segments.len() {
        for j in (i + 1)..segments.len() {
            if let Some(p) = segment_intersection(segments[i], segments[j]) {
                candidate(DVec2::new(p[0], p[1]), SnapTarget::Intersection);
            }
        }
    }

    // Grid snap.
    if grid_mm > 0.0 {
        let gx = (q.x / grid_mm).round() * grid_mm;
        let gy = (q.y / grid_mm).round() * grid_mm;
        candidate(DVec2::new(gx, gy), SnapTarget::Grid);
    }

    best
}

fn segment_intersection(
    s1: ([f64; 2], [f64; 2]),
    s2: ([f64; 2], [f64; 2]),
) -> Option<[f64; 2]> {
    let p = DVec2::new(s1.0[0], s1.0[1]);
    let r = DVec2::new(s1.1[0] - s1.0[0], s1.1[1] - s1.0[1]);
    let q = DVec2::new(s2.0[0], s2.0[1]);
    let s = DVec2::new(s2.1[0] - s2.0[0], s2.1[1] - s2.0[1]);
    let rxs = r.x * s.y - r.y * s.x;
    if rxs.abs() < 1e-9 {
        return None;
    }
    let qp = q - p;
    let t = (qp.x * s.y - qp.y * s.x) / rxs;
    let u = (qp.x * r.y - qp.y * r.x) / rxs;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some([p.x + r.x * t, p.y + r.y * t])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_snap_wins_over_grid_when_close() {
        let seg = ([1234.0, 5678.0], [4321.0, 8765.0]);
        let res = snap_to([1235.0, 5679.0], &[seg], 100.0, 50.0).unwrap();
        assert_eq!(res.target, SnapTarget::Endpoint);
    }

    #[test]
    fn grid_snap_used_when_no_segment_nearby() {
        let res = snap_to([97.0, 102.0], &[], 100.0, 50.0).unwrap();
        assert_eq!(res.target, SnapTarget::Grid);
        assert_eq!(res.point_mm, [100.0, 100.0]);
    }

    #[test]
    fn out_of_tolerance_returns_none() {
        let res = snap_to([1000.0, 1000.0], &[], 0.0, 50.0);
        assert!(res.is_none());
    }

    #[test]
    fn intersection_snap_is_found() {
        let s1 = ([0.0, 0.0], [10.0, 10.0]);
        let s2 = ([0.0, 10.0], [10.0, 0.0]);
        let res = snap_to([5.1, 5.1], &[s1, s2], 0.0, 0.5).unwrap();
        assert_eq!(res.target, SnapTarget::Intersection);
        assert!((res.point_mm[0] - 5.0).abs() < 1e-6);
        assert!((res.point_mm[1] - 5.0).abs() < 1e-6);
    }
}
