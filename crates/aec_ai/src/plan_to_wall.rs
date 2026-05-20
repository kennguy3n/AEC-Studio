//! Plan → wall conversion.
//!
//! Takes detected polylines (from `plan_detection`) and converts them
//! into parametric walls with thickness, start/end caps, and a layer
//! assignment. Adjacent wall axes that share an endpoint are joined to
//! produce a clean topology (a graph of axes with junction-aware caps).

use serde::{Deserialize, Serialize};

use crate::plan_detection::{PlanDetectionResult, PolylineProposal};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WallAxisSegment {
    /// Start point (mm).
    pub start: [f64; 2],
    /// End point (mm).
    pub end: [f64; 2],
}

impl WallAxisSegment {
    pub fn length(&self) -> f64 {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        (dx * dx + dy * dy).sqrt()
    }
}

/// How the end of a wall axis is closed off (visually) when it doesn't
/// connect to another wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallEndCap {
    Open,
    Square,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wall {
    /// Stable id assigned during conversion (1-based, dense).
    pub id: u32,
    pub axis: WallAxisSegment,
    pub thickness: f64,
    pub start_cap: WallEndCap,
    pub end_cap: WallEndCap,
    pub layer: String,
    pub source_confidence: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanToWallConfig {
    /// Default wall thickness (mm).
    pub default_thickness: f64,
    /// Tolerance for considering two endpoints "the same" (mm).
    pub junction_tolerance: f64,
    /// Lower bound for a wall axis length; shorter segments are dropped
    /// as detection noise.
    pub min_segment_length: f64,
    /// Drop wall proposals whose detection confidence is below this.
    pub min_confidence: f32,
    /// Target layer for produced walls.
    pub layer: &'static str,
}

impl Default for PlanToWallConfig {
    fn default() -> Self {
        Self {
            default_thickness: 100.0,
            junction_tolerance: 25.0,
            min_segment_length: 100.0,
            min_confidence: 0.5,
            layer: "A-WALL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanToWallResult {
    pub walls: Vec<Wall>,
    /// Number of polylines dropped (below confidence or length).
    pub dropped: usize,
}

/// Walk each polyline as a sequence of consecutive segments, then build
/// walls. Segments shorter than `min_segment_length` are skipped.
pub fn convert(input: &PlanDetectionResult, cfg: &PlanToWallConfig) -> PlanToWallResult {
    let mut segments: Vec<(WallAxisSegment, f32)> = Vec::new();
    let mut dropped = 0;
    for p in &input.polylines {
        if p.confidence < cfg.min_confidence || p.points_mm.len() < 2 {
            dropped += 1;
            continue;
        }
        for win in p.points_mm.windows(2) {
            let seg = WallAxisSegment {
                start: win[0],
                end: win[1],
            };
            if seg.length() >= cfg.min_segment_length {
                segments.push((seg, p.confidence));
            }
        }
    }

    // Determine end-cap policy: an end is "open" if it touches another
    // segment's endpoint (within junction tolerance).
    let mut walls = Vec::with_capacity(segments.len());
    for (idx, (seg, conf)) in segments.iter().enumerate() {
        let start_cap =
            if touches_any_other_endpoint(&segments, idx, seg.start, cfg.junction_tolerance) {
                WallEndCap::Open
            } else {
                WallEndCap::Square
            };
        let end_cap = if touches_any_other_endpoint(&segments, idx, seg.end, cfg.junction_tolerance)
        {
            WallEndCap::Open
        } else {
            WallEndCap::Square
        };
        walls.push(Wall {
            id: (idx as u32) + 1,
            axis: *seg,
            thickness: cfg.default_thickness,
            start_cap,
            end_cap,
            layer: cfg.layer.to_string(),
            source_confidence: *conf,
        });
    }

    PlanToWallResult { walls, dropped }
}

fn touches_any_other_endpoint(
    segments: &[(WallAxisSegment, f32)],
    skip: usize,
    p: [f64; 2],
    tol: f64,
) -> bool {
    for (i, (seg, _)) in segments.iter().enumerate() {
        if i == skip {
            continue;
        }
        if approx_equal(p, seg.start, tol) || approx_equal(p, seg.end, tol) {
            return true;
        }
    }
    false
}

fn approx_equal(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt() <= tol
}

/// Convenience: build directly from a list of `PolylineProposal`s.
pub fn convert_from_polylines(
    polylines: Vec<PolylineProposal>,
    cfg: &PlanToWallConfig,
) -> PlanToWallResult {
    convert(&PlanDetectionResult { polylines }, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly(points: Vec<[f64; 2]>, conf: f32) -> PolylineProposal {
        PolylineProposal {
            points_mm: points,
            confidence: conf,
        }
    }

    #[test]
    fn single_polyline_becomes_walls_per_segment() {
        let inp = PlanDetectionResult {
            polylines: vec![poly(vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0]], 0.9)],
        };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert_eq!(r.walls.len(), 2);
        // First wall horizontal.
        assert!((r.walls[0].axis.length() - 4000.0).abs() < 1e-6);
        // Second wall vertical.
        assert!((r.walls[1].axis.length() - 3000.0).abs() < 1e-6);
    }

    #[test]
    fn shared_endpoint_makes_open_caps_at_junction() {
        let inp = PlanDetectionResult {
            polylines: vec![poly(vec![[0.0, 0.0], [4000.0, 0.0], [4000.0, 3000.0]], 0.9)],
        };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert_eq!(r.walls.len(), 2);
        // Wall 1 end is shared with wall 2 start.
        assert_eq!(r.walls[0].end_cap, WallEndCap::Open);
        assert_eq!(r.walls[1].start_cap, WallEndCap::Open);
        // Wall 1 start is not shared.
        assert_eq!(r.walls[0].start_cap, WallEndCap::Square);
        assert_eq!(r.walls[1].end_cap, WallEndCap::Square);
    }

    #[test]
    fn low_confidence_is_dropped() {
        let inp = PlanDetectionResult {
            polylines: vec![poly(vec![[0.0, 0.0], [4000.0, 0.0]], 0.2)],
        };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert!(r.walls.is_empty());
        assert_eq!(r.dropped, 1);
    }

    #[test]
    fn short_segments_filtered() {
        let inp = PlanDetectionResult {
            polylines: vec![poly(vec![[0.0, 0.0], [50.0, 0.0]], 0.9)],
        };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert!(r.walls.is_empty());
    }

    #[test]
    fn deterministic_ids_and_layer() {
        let inp = PlanDetectionResult {
            polylines: vec![
                poly(vec![[0.0, 0.0], [4000.0, 0.0]], 0.9),
                poly(vec![[0.0, 1000.0], [4000.0, 1000.0]], 0.85),
            ],
        };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert_eq!(r.walls[0].id, 1);
        assert_eq!(r.walls[1].id, 2);
        assert_eq!(r.walls[0].layer, "A-WALL");
    }

    #[test]
    fn config_can_override_layer_and_thickness() {
        let inp = PlanDetectionResult {
            polylines: vec![poly(vec![[0.0, 0.0], [4000.0, 0.0]], 0.9)],
        };
        let cfg = PlanToWallConfig {
            default_thickness: 150.0,
            layer: "A-WALL-EXT",
            ..PlanToWallConfig::default()
        };
        let r = convert(&inp, &cfg);
        assert_eq!(r.walls[0].layer, "A-WALL-EXT");
        assert!((r.walls[0].thickness - 150.0).abs() < 1e-9);
    }

    #[test]
    fn empty_input_yields_no_walls() {
        let inp = PlanDetectionResult { polylines: vec![] };
        let r = convert(&inp, &PlanToWallConfig::default());
        assert!(r.walls.is_empty());
        assert_eq!(r.dropped, 0);
    }

    #[test]
    fn wall_serializes_round_trip() {
        let w = Wall {
            id: 1,
            axis: WallAxisSegment {
                start: [0.0, 0.0],
                end: [4000.0, 0.0],
            },
            thickness: 100.0,
            start_cap: WallEndCap::Square,
            end_cap: WallEndCap::Open,
            layer: "A-WALL".into(),
            source_confidence: 0.9,
        };
        let s = serde_json::to_string(&w).unwrap();
        let r: Wall = serde_json::from_str(&s).unwrap();
        assert_eq!(w, r);
    }
}
