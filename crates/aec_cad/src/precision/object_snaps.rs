//! Object snap engine — generate candidate snap points from a set of
//! primitives, filtered by enabled snap modes and an aperture radius.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::editing::trim_tool::line_line_intersection;
use crate::primitives::{Primitive, SnapKind, SnapPoint};

bitflags! {
    /// Bitmask of enabled snap modes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct SnapModes: u32 {
        const ENDPOINT      = 1 << 0;
        const MIDPOINT      = 1 << 1;
        const CENTER        = 1 << 2;
        const INTERSECTION  = 1 << 3;
        const PERPENDICULAR = 1 << 4;
        const TANGENT       = 1 << 5;
        const NEAREST       = 1 << 6;
        const NODE          = 1 << 7;
        const QUADRANT      = 1 << 8;
        const INSERTION     = 1 << 9;
    }
}

impl Default for SnapModes {
    fn default() -> Self {
        Self::ENDPOINT
            | Self::MIDPOINT
            | Self::CENTER
            | Self::INTERSECTION
            | Self::NODE
            | Self::QUADRANT
            | Self::INSERTION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SnapHit {
    pub kind: SnapKind,
    pub at: [f64; 2],
    pub distance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectSnapEngine {
    pub modes: SnapModes,
    pub aperture: f64,
}

impl Default for ObjectSnapEngine {
    fn default() -> Self {
        Self {
            modes: SnapModes::default(),
            aperture: 5.0,
        }
    }
}

fn kind_allowed(modes: SnapModes, kind: SnapKind) -> bool {
    match kind {
        SnapKind::Endpoint => modes.contains(SnapModes::ENDPOINT),
        SnapKind::Midpoint => modes.contains(SnapModes::MIDPOINT),
        SnapKind::Center => modes.contains(SnapModes::CENTER),
        SnapKind::Node => modes.contains(SnapModes::NODE),
        SnapKind::Quadrant => modes.contains(SnapModes::QUADRANT),
        SnapKind::Insertion => modes.contains(SnapModes::INSERTION),
        SnapKind::Nearest => modes.contains(SnapModes::NEAREST),
    }
}

impl ObjectSnapEngine {
    pub fn snap(&self, query: [f64; 2], primitives: &[Primitive]) -> Option<SnapHit> {
        let mut candidates: Vec<SnapHit> = Vec::new();
        for prim in primitives {
            for SnapPoint { kind, at } in prim.snap_points() {
                if !kind_allowed(self.modes, kind) {
                    continue;
                }
                let d = ((query[0] - at[0]).powi(2) + (query[1] - at[1]).powi(2)).sqrt();
                if d <= self.aperture {
                    candidates.push(SnapHit {
                        kind,
                        at,
                        distance: d,
                    });
                }
            }
        }
        // Pairwise intersections of lines.
        if self.modes.contains(SnapModes::INTERSECTION) {
            for (i, a) in primitives.iter().enumerate() {
                for b in primitives.iter().skip(i + 1) {
                    if let (Primitive::Line(la), Primitive::Line(lb)) = (a, b) {
                        if let Some(hit) = line_line_intersection(la, lb) {
                            let d =
                                ((query[0] - hit[0]).powi(2) + (query[1] - hit[1]).powi(2)).sqrt();
                            if d <= self.aperture {
                                candidates.push(SnapHit {
                                    kind: SnapKind::Endpoint,
                                    at: hit,
                                    distance: d,
                                });
                            }
                        }
                    }
                }
            }
        }
        // Nearest snap — fallback projection onto any primitive (lowest priority).
        if self.modes.contains(SnapModes::NEAREST) {
            for prim in primitives {
                let d2 = prim.distance2(query);
                let d = d2.sqrt();
                if d <= self.aperture {
                    // Compute the actual projected point for lines/polylines.
                    if let Primitive::Line(l) = prim {
                        let c = l.closest_point(query);
                        candidates.push(SnapHit {
                            kind: SnapKind::Nearest,
                            at: c,
                            distance: d,
                        });
                    }
                }
            }
        }
        candidates.into_iter().min_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{Circle, Line};

    #[test]
    fn snaps_to_endpoint_when_within_aperture() {
        let line = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        let engine = ObjectSnapEngine::default();
        let hit = engine.snap([0.3, 0.0], &[line]).unwrap();
        assert_eq!(hit.kind, SnapKind::Endpoint);
        assert_eq!(hit.at, [0.0, 0.0]);
    }

    #[test]
    fn snaps_to_circle_center() {
        let c = Primitive::Circle(Circle::new("0", [3.0, 4.0], 2.0));
        let engine = ObjectSnapEngine::default();
        let hit = engine.snap([3.1, 4.0], &[c]).unwrap();
        assert_eq!(hit.kind, SnapKind::Center);
        assert_eq!(hit.at, [3.0, 4.0]);
    }

    #[test]
    fn snaps_to_intersection_of_two_lines() {
        let a = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        let b = Primitive::Line(Line::new("0", [5.0, -5.0], [5.0, 5.0]));
        let engine = ObjectSnapEngine::default();
        let hit = engine.snap([5.2, 0.1], &[a, b]).unwrap();
        assert_eq!(hit.at, [5.0, 0.0]);
    }

    #[test]
    fn outside_aperture_returns_none() {
        let line = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        let engine = ObjectSnapEngine::default();
        assert!(engine.snap([100.0, 100.0], &[line]).is_none());
    }
}
