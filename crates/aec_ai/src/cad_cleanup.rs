//! CAD cleanup — local algorithms that an AI tool call can invoke
//! and previewable diffs the safety validator can show.
//!
//! Five real cleanup operations are implemented:
//!
//! 1. `close_gaps` — find near-miss polyline endpoints and snap them
//!    together (within a configurable tolerance).
//! 2. `dedupe_entities` — detect duplicate / overlapping line entities
//!    (within a tolerance) and return the keep-set + drop-set.
//! 3. `merge_collinear` — merge two lines that are collinear and share
//!    an endpoint into a single line.
//! 4. `normalize_layers` — propose layer reassignments based on a
//!    layer-policy table mapping geometry property predicates to a
//!    target layer.
//! 5. `cleanup_proposal` — orchestrates 1–4 into a single
//!    `CleanupProposal` that produces a previewable diff plan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LineEntity {
    pub id: u64,
    pub layer_id: u32,
    pub start: [f64; 2],
    pub end: [f64; 2],
}

impl LineEntity {
    pub fn length(&self) -> f64 {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        (dx * dx + dy * dy).sqrt()
    }

    pub fn direction(&self) -> [f64; 2] {
        let dx = self.end[0] - self.start[0];
        let dy = self.end[1] - self.start[1];
        let l = (dx * dx + dy * dy).sqrt();
        if l < f64::EPSILON {
            [0.0, 0.0]
        } else {
            [dx / l, dy / l]
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CleanupConfig {
    /// Tolerance for gap closing (in model units, e.g. mm).
    pub gap_tolerance: f64,
    /// Tolerance for duplicate detection.
    pub dedupe_tolerance: f64,
    /// Tolerance for collinearity (cross-product magnitude).
    pub collinearity_tolerance: f64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            gap_tolerance: 5.0,
            dedupe_tolerance: 0.1,
            collinearity_tolerance: 1e-3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapClosure {
    pub line_a: u64,
    pub line_b: u64,
    /// Which endpoint of line_a (0 = start, 1 = end) was snapped.
    pub endpoint_a: u8,
    /// Same for line_b.
    pub endpoint_b: u8,
    pub from: [f64; 2],
    pub to: [f64; 2],
    pub distance: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// IDs in the group; the smallest ID is kept, the rest dropped.
    pub members: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollinearMerge {
    pub line_a: u64,
    pub line_b: u64,
    /// Resulting merged line.
    pub merged_start: [f64; 2],
    pub merged_end: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerReassignment {
    pub line_id: u64,
    pub from_layer: u32,
    pub to_layer: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CleanupProposal {
    pub gap_closures: Vec<GapClosure>,
    pub duplicate_groups: Vec<DuplicateGroup>,
    pub collinear_merges: Vec<CollinearMerge>,
    pub layer_reassignments: Vec<LayerReassignment>,
}

impl CleanupProposal {
    pub fn is_empty(&self) -> bool {
        self.gap_closures.is_empty()
            && self.duplicate_groups.is_empty()
            && self.collinear_merges.is_empty()
            && self.layer_reassignments.is_empty()
    }

    pub fn entity_count_modified(&self) -> usize {
        // Each gap closure touches two endpoints; each duplicate group
        // drops (members - 1) entities; each merge touches 2; each
        // reassignment touches 1.
        let mut n = 0;
        n += self.gap_closures.len() * 2;
        n += self
            .duplicate_groups
            .iter()
            .map(|g| g.members.len().saturating_sub(1))
            .sum::<usize>();
        n += self.collinear_merges.len() * 2;
        n += self.layer_reassignments.len();
        n
    }
}

/// Find polyline endpoints within `cfg.gap_tolerance` and propose snaps.
pub fn close_gaps(lines: &[LineEntity], cfg: &CleanupConfig) -> Vec<GapClosure> {
    let mut out = Vec::new();
    for i in 0..lines.len() {
        for j in (i + 1)..lines.len() {
            let a = &lines[i];
            let b = &lines[j];
            for (idx_a, pa) in [(0u8, a.start), (1u8, a.end)] {
                for (idx_b, pb) in [(0u8, b.start), (1u8, b.end)] {
                    let dx = pa[0] - pb[0];
                    let dy = pa[1] - pb[1];
                    let d = (dx * dx + dy * dy).sqrt();
                    if d > 0.0 && d <= cfg.gap_tolerance {
                        out.push(GapClosure {
                            line_a: a.id,
                            line_b: b.id,
                            endpoint_a: idx_a,
                            endpoint_b: idx_b,
                            from: pa,
                            to: [(pa[0] + pb[0]) * 0.5, (pa[1] + pb[1]) * 0.5],
                            distance: d,
                        });
                    }
                }
            }
        }
    }
    out
}

fn approx_equal(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
    (a[0] - b[0]).abs() <= tol && (a[1] - b[1]).abs() <= tol
}

/// Group line entities that are duplicates (same endpoints either way
/// around, within tolerance).
pub fn dedupe_entities(lines: &[LineEntity], cfg: &CleanupConfig) -> Vec<DuplicateGroup> {
    let mut groups: Vec<Vec<u64>> = Vec::new();
    let mut seen: Vec<bool> = vec![false; lines.len()];
    for i in 0..lines.len() {
        if seen[i] {
            continue;
        }
        let mut group = vec![lines[i].id];
        seen[i] = true;
        for j in (i + 1)..lines.len() {
            if seen[j] {
                continue;
            }
            let a = &lines[i];
            let b = &lines[j];
            let forward = approx_equal(a.start, b.start, cfg.dedupe_tolerance)
                && approx_equal(a.end, b.end, cfg.dedupe_tolerance);
            let reversed = approx_equal(a.start, b.end, cfg.dedupe_tolerance)
                && approx_equal(a.end, b.start, cfg.dedupe_tolerance);
            if forward || reversed {
                group.push(b.id);
                seen[j] = true;
            }
        }
        if group.len() > 1 {
            group.sort_unstable();
            groups.push(group);
        }
    }
    groups
        .into_iter()
        .map(|members| DuplicateGroup { members })
        .collect()
}

/// Find pairs of lines that are collinear and share a single endpoint;
/// propose merging them into one line.
pub fn merge_collinear(lines: &[LineEntity], cfg: &CleanupConfig) -> Vec<CollinearMerge> {
    let mut out = Vec::new();
    let mut used = vec![false; lines.len()];
    for i in 0..lines.len() {
        if used[i] {
            continue;
        }
        for j in (i + 1)..lines.len() {
            if used[j] {
                continue;
            }
            let a = &lines[i];
            let b = &lines[j];
            // Find shared endpoint.
            let pairs = [
                (a.start, a.end, b.start, b.end),
                (a.start, a.end, b.end, b.start),
                (a.end, a.start, b.start, b.end),
                (a.end, a.start, b.end, b.start),
            ];
            for (a_far, a_shared, b_shared, b_far) in pairs {
                if approx_equal(a_shared, b_shared, cfg.dedupe_tolerance) {
                    // Check collinearity via cross product of directions.
                    let d1 = [a_shared[0] - a_far[0], a_shared[1] - a_far[1]];
                    let d2 = [b_far[0] - b_shared[0], b_far[1] - b_shared[1]];
                    let cross = d1[0] * d2[1] - d1[1] * d2[0];
                    let l1 = (d1[0] * d1[0] + d1[1] * d1[1]).sqrt();
                    let l2 = (d2[0] * d2[0] + d2[1] * d2[1]).sqrt();
                    if l1 < f64::EPSILON || l2 < f64::EPSILON {
                        continue;
                    }
                    if (cross / (l1 * l2)).abs() < cfg.collinearity_tolerance {
                        out.push(CollinearMerge {
                            line_a: a.id,
                            line_b: b.id,
                            merged_start: a_far,
                            merged_end: b_far,
                        });
                        used[i] = true;
                        used[j] = true;
                        break;
                    }
                }
            }
            if used[i] {
                break;
            }
        }
    }
    out
}

/// A rule that says "lines matching `predicate` belong on layer
/// `target_layer`, because `reason`".
#[derive(Debug, Clone)]
pub struct LayerPolicyRule {
    pub target_layer: u32,
    pub reason: String,
    pub predicate: fn(&LineEntity) -> bool,
}

pub fn normalize_layers(
    lines: &[LineEntity],
    policy: &[LayerPolicyRule],
) -> Vec<LayerReassignment> {
    let mut out = Vec::new();
    for line in lines {
        for rule in policy {
            if (rule.predicate)(line) && line.layer_id != rule.target_layer {
                out.push(LayerReassignment {
                    line_id: line.id,
                    from_layer: line.layer_id,
                    to_layer: rule.target_layer,
                    reason: rule.reason.clone(),
                });
                break;
            }
        }
    }
    out
}

/// Run all cleanup operations against the given input.
pub fn build_cleanup_proposal(
    lines: &[LineEntity],
    cfg: &CleanupConfig,
    layer_policy: &[LayerPolicyRule],
) -> CleanupProposal {
    CleanupProposal {
        gap_closures: close_gaps(lines, cfg),
        duplicate_groups: dedupe_entities(lines, cfg),
        collinear_merges: merge_collinear(lines, cfg),
        layer_reassignments: normalize_layers(lines, layer_policy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u64, layer: u32, a: [f64; 2], b: [f64; 2]) -> LineEntity {
        LineEntity {
            id,
            layer_id: layer,
            start: a,
            end: b,
        }
    }

    #[test]
    fn close_gaps_detects_near_endpoints() {
        let lines = vec![
            line(1, 0, [0.0, 0.0], [100.0, 0.0]),
            // 0.5-mm gap → within tolerance.
            line(2, 0, [100.5, 0.0], [200.0, 0.0]),
        ];
        let g = close_gaps(&lines, &CleanupConfig::default());
        assert_eq!(g.len(), 1);
        assert!((g[0].distance - 0.5).abs() < 1e-9);
        // Snapped midpoint.
        assert!((g[0].to[0] - 100.25).abs() < 1e-9);
    }

    #[test]
    fn close_gaps_ignores_far_endpoints() {
        let lines = vec![
            line(1, 0, [0.0, 0.0], [100.0, 0.0]),
            line(2, 0, [200.0, 0.0], [300.0, 0.0]),
        ];
        let g = close_gaps(&lines, &CleanupConfig::default());
        assert!(g.is_empty());
    }

    #[test]
    fn dedupe_detects_reversed_duplicate() {
        let lines = vec![
            line(1, 0, [0.0, 0.0], [10.0, 0.0]),
            // Same line, but ID 2 and reversed direction.
            line(2, 0, [10.0, 0.0], [0.0, 0.0]),
        ];
        let groups = dedupe_entities(&lines, &CleanupConfig::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![1, 2]);
    }

    #[test]
    fn dedupe_keeps_unique_lines_separate() {
        let lines = vec![
            line(1, 0, [0.0, 0.0], [10.0, 0.0]),
            line(2, 0, [0.0, 1.0], [10.0, 1.0]),
        ];
        let groups = dedupe_entities(&lines, &CleanupConfig::default());
        assert!(groups.is_empty());
    }

    #[test]
    fn collinear_merge_picks_correct_far_endpoints() {
        // Three collinear x-axis segments meeting at (5,0) and (10,0).
        let lines = vec![
            line(1, 0, [0.0, 0.0], [5.0, 0.0]),
            line(2, 0, [5.0, 0.0], [10.0, 0.0]),
        ];
        let merges = merge_collinear(&lines, &CleanupConfig::default());
        assert_eq!(merges.len(), 1);
        assert!((merges[0].merged_start[0] - 0.0).abs() < 1e-9);
        assert!((merges[0].merged_end[0] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn collinear_merge_skips_non_collinear() {
        let lines = vec![
            line(1, 0, [0.0, 0.0], [5.0, 0.0]),
            line(2, 0, [5.0, 0.0], [5.0, 5.0]), // perpendicular
        ];
        let merges = merge_collinear(&lines, &CleanupConfig::default());
        assert!(merges.is_empty());
    }

    #[test]
    fn normalize_layers_proposes_moves() {
        let lines = vec![
            // long line currently on layer 0
            line(1, 0, [0.0, 0.0], [10000.0, 0.0]),
            // short line currently on layer 0
            line(2, 0, [0.0, 0.0], [50.0, 0.0]),
        ];
        let policy = vec![
            LayerPolicyRule {
                target_layer: 1,
                reason: "walls > 1m".into(),
                predicate: |l| l.length() > 1000.0,
            },
            LayerPolicyRule {
                target_layer: 2,
                reason: "details < 100mm".into(),
                predicate: |l| l.length() < 100.0,
            },
        ];
        let r = normalize_layers(&lines, &policy);
        assert_eq!(r.len(), 2);
        // First rule applies to long line.
        assert_eq!(r[0].line_id, 1);
        assert_eq!(r[0].to_layer, 1);
        // Second rule applies to short line.
        assert_eq!(r[1].line_id, 2);
        assert_eq!(r[1].to_layer, 2);
    }

    #[test]
    fn normalize_layers_skips_already_on_target() {
        let lines = vec![line(1, 1, [0.0, 0.0], [10000.0, 0.0])];
        let policy = vec![LayerPolicyRule {
            target_layer: 1,
            reason: "walls".into(),
            predicate: |l| l.length() > 1000.0,
        }];
        let r = normalize_layers(&lines, &policy);
        assert!(r.is_empty());
    }

    #[test]
    fn cleanup_proposal_counts_modified_entities() {
        let p = CleanupProposal {
            gap_closures: vec![GapClosure {
                line_a: 1,
                line_b: 2,
                endpoint_a: 1,
                endpoint_b: 0,
                from: [0.0, 0.0],
                to: [0.1, 0.0],
                distance: 0.1,
            }],
            duplicate_groups: vec![DuplicateGroup {
                members: vec![3, 4, 5],
            }],
            collinear_merges: vec![CollinearMerge {
                line_a: 6,
                line_b: 7,
                merged_start: [0.0, 0.0],
                merged_end: [10.0, 0.0],
            }],
            layer_reassignments: vec![LayerReassignment {
                line_id: 8,
                from_layer: 0,
                to_layer: 1,
                reason: "walls".into(),
            }],
        };
        // 2 (gap) + 2 (dup keep 1 of 3) + 2 (merge) + 1 (reassign) = 7
        assert_eq!(p.entity_count_modified(), 7);
    }

    #[test]
    fn cleanup_proposal_is_serializable() {
        let p = CleanupProposal::default();
        let s = serde_json::to_string(&p).unwrap();
        let r: CleanupProposal = serde_json::from_str(&s).unwrap();
        assert_eq!(p, r);
    }
}
