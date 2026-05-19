//! Visual snap indicators: endpoint dots, midpoint marks, intersection
//! crosses, grid dots. The overlay does the *math* — the actual rendering
//! is a polyline node in the scene graph.

use glam::Vec3;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapKind {
    Endpoint,
    Midpoint,
    Intersection,
    Perpendicular,
    Grid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapHit {
    pub kind: SnapKind,
    pub position: [f32; 3],
    pub distance: f32,
}

pub struct SnapOverlay {
    pub max_distance_mm: f32,
}

impl SnapOverlay {
    pub fn new() -> Self {
        Self {
            max_distance_mm: 80.0,
        }
    }

    /// Snap `cursor` to the nearest candidate point. Returns the closest
    /// candidate within `max_distance_mm`, or `None`.
    pub fn snap_to_candidates(&self, cursor: Vec3, candidates: &[SnapHit]) -> Option<SnapHit> {
        let mut best: Option<SnapHit> = None;
        for c in candidates {
            let d = (Vec3::from(c.position) - cursor).length();
            if d <= self.max_distance_mm {
                match &best {
                    Some(b) if d >= b.distance => {}
                    _ => {
                        best = Some(SnapHit {
                            distance: d,
                            ..c.clone()
                        });
                    }
                }
            }
        }
        best
    }
}

impl Default for SnapOverlay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_nearest_candidate_within_range() {
        let overlay = SnapOverlay::new();
        let candidates = vec![
            SnapHit {
                kind: SnapKind::Endpoint,
                position: [100.0, 0.0, 100.0],
                distance: 0.0,
            },
            SnapHit {
                kind: SnapKind::Midpoint,
                position: [50.0, 0.0, 50.0],
                distance: 0.0,
            },
            SnapHit {
                kind: SnapKind::Grid,
                position: [0.0, 0.0, 0.0],
                distance: 0.0,
            },
        ];
        let hit = overlay
            .snap_to_candidates(Vec3::new(48.0, 0.0, 48.0), &candidates)
            .unwrap();
        assert_eq!(hit.kind, SnapKind::Midpoint);
    }

    #[test]
    fn returns_none_when_out_of_range() {
        let overlay = SnapOverlay::new();
        let candidates = vec![SnapHit {
            kind: SnapKind::Endpoint,
            position: [10_000.0, 0.0, 10_000.0],
            distance: 0.0,
        }];
        assert!(overlay
            .snap_to_candidates(Vec3::ZERO, &candidates)
            .is_none());
    }
}
