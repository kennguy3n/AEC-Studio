//! Object snap tracking — generate construction lines from previously
//! acquired snap points so the user can align to snap positions that
//! aren't directly under the cursor.

use serde::{Deserialize, Serialize};

use crate::primitives::SnapPoint;

/// A construction line emitted by the tracking engine. The UI draws it
/// as a dotted line through `origin` in direction `angle_deg`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackLine {
    pub origin: [f64; 2],
    pub angle_deg: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackingEngine {
    pub enabled: bool,
    acquired: Vec<SnapPoint>,
}

impl TrackingEngine {
    pub fn clear(&mut self) {
        self.acquired.clear();
    }

    pub fn acquire(&mut self, pt: SnapPoint) {
        if !self.acquired.iter().any(|p| p.at == pt.at) {
            self.acquired.push(pt);
        }
    }

    /// Return horizontal + vertical track lines from every acquired snap.
    pub fn track_lines(&self) -> Vec<TrackLine> {
        let mut out = Vec::with_capacity(self.acquired.len() * 2);
        for pt in &self.acquired {
            out.push(TrackLine {
                origin: pt.at,
                angle_deg: 0.0,
            });
            out.push(TrackLine {
                origin: pt.at,
                angle_deg: 90.0,
            });
        }
        out
    }

    /// Try snapping `cursor` to the closest tracking construction line.
    /// Returns `None` when tracking is off or nothing is close.
    pub fn snap(&self, cursor: [f64; 2], aperture: f64) -> Option<[f64; 2]> {
        if !self.enabled || self.acquired.is_empty() {
            return None;
        }
        let mut best: Option<([f64; 2], f64)> = None;
        for pt in &self.acquired {
            // Horizontal track: y=pt.y.
            let d_h = (cursor[1] - pt.at[1]).abs();
            if d_h <= aperture {
                let cand = [cursor[0], pt.at[1]];
                if best.is_none() || d_h < best.unwrap().1 {
                    best = Some((cand, d_h));
                }
            }
            // Vertical track: x=pt.x.
            let d_v = (cursor[0] - pt.at[0]).abs();
            if d_v <= aperture {
                let cand = [pt.at[0], cursor[1]];
                if best.is_none() || d_v < best.unwrap().1 {
                    best = Some((cand, d_v));
                }
            }
        }
        best.map(|(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::SnapKind;

    fn sp(x: f64, y: f64) -> SnapPoint {
        SnapPoint {
            kind: SnapKind::Endpoint,
            at: [x, y],
        }
    }

    #[test]
    fn track_lines_from_acquired() {
        let mut e = TrackingEngine {
            enabled: true,
            ..TrackingEngine::default()
        };
        e.acquire(sp(10.0, 5.0));
        let lines = e.track_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].angle_deg, 0.0);
        assert_eq!(lines[1].angle_deg, 90.0);
    }

    #[test]
    fn snap_to_horizontal_track() {
        let mut e = TrackingEngine {
            enabled: true,
            ..TrackingEngine::default()
        };
        e.acquire(sp(10.0, 5.0));
        let r = e.snap([20.0, 5.1], 1.0).unwrap();
        assert!((r[1] - 5.0).abs() < 1e-9);
    }

    #[test]
    fn disabled_engine_returns_none() {
        let mut e = TrackingEngine::default();
        e.acquire(sp(10.0, 5.0));
        assert!(e.snap([10.1, 5.0], 1.0).is_none());
    }
}
