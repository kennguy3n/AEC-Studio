//! Local-AI BIM classification.
//!
//! Takes a list of unclassified mesh proposals (each described by a
//! few geometric features) and produces a classification proposal
//! keyed by entity id. The proposal is intentionally structured to
//! match the `classification` grammar so it can be served either by
//! the sidecar with grammar-constrained decoding or by a heuristic
//! deterministic fallback that lives entirely in Rust (used in unit
//! tests and offline).
//!
//! The deterministic fallback uses simple geometric rules that match
//! how an architect would describe each class:
//!
//! * IfcWall — long, thin, vertical box (length ≫ thickness, large
//!   height, ≥ 0.6 m off the ground span)
//! * IfcSlab — thin horizontal box (large XY area, small Z extent)
//! * IfcColumn — short footprint area, large height (height / width
//!   > 4)
//! * IfcBeam — long thin horizontal box (length ≫ width, modest
//!   height, elevated above floor)
//! * IfcDoor — short height box (0.8–2.4 m wide × 1.8–2.4 m tall)
//!   that opens through a wall (parent is a wall)
//! * IfcWindow — similar to door but typically wider/shorter and
//!   higher off the floor
//! * IfcRoof — large XY area at top of building
//! * IfcStair — non-axis-aligned, tall, multi-level
//!
//! Anything that doesn't match a rule falls through to
//! `IfcBuildingElementProxy`. The confidence is calibrated so good
//! matches sit > 0.85 and ambiguous matches sit below 0.85 — which
//! integrates cleanly with `ClassificationStore::accept_threshold`.

use serde::{Deserialize, Serialize};

use aec_bim::classification::IfcClass;
use aec_core::types::{DiffId, EntityId};

use crate::diff_engine::{Diff, DiffOperation, DiffStatus};
use crate::tool_schema::ToolName;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryFeatures {
    /// Stable string id for the mesh / element. Echoed back in the
    /// proposal so the caller can re-key it.
    pub entity: String,
    /// Axis-aligned bounding box extents (m).
    pub size: [f64; 3],
    /// Centroid (m). z is height above project base.
    pub centroid: [f64; 3],
    /// Optional parent spatial class as a hint ("IfcWall" suggests
    /// children are likely doors/windows).
    #[serde(default)]
    pub parent_class: Option<String>,
    /// Whether the mesh has openings cut into it (relevant for walls
    /// vs slabs).
    #[serde(default)]
    pub has_openings: bool,
}

impl GeometryFeatures {
    pub fn footprint_area(&self) -> f64 {
        self.size[0] * self.size[1]
    }
    pub fn longest_horizontal(&self) -> f64 {
        self.size[0].max(self.size[1])
    }
    pub fn shortest_horizontal(&self) -> f64 {
        self.size[0].min(self.size[1])
    }
    pub fn height(&self) -> f64 {
        self.size[2]
    }
    pub fn aspect_h_over_w(&self) -> f64 {
        if self.shortest_horizontal() < 1e-6 {
            return f64::MAX;
        }
        self.height() / self.shortest_horizontal()
    }
    pub fn aspect_length_over_width(&self) -> f64 {
        if self.shortest_horizontal() < 1e-6 {
            return f64::MAX;
        }
        self.longest_horizontal() / self.shortest_horizontal()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationProposal {
    pub entity: String,
    pub ifc_class: IfcClass,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassificationResult {
    pub assignments: Vec<ClassificationProposal>,
}

impl ClassificationResult {
    /// Build a previewable [`Diff`] from this result. Each accepted
    /// proposal becomes an `Update` operation patching the element's
    /// `class` field. Proposals whose `entity` is not a valid
    /// `EntityId` are skipped — we never invent fresh ids here.
    pub fn to_diff(&self) -> Diff {
        let mut ops = Vec::new();
        for a in &self.assignments {
            let Ok(target) = a.entity.parse::<EntityId>() else {
                continue;
            };
            let patch = serde_json::json!({
                "ifc_class": a.ifc_class.ifc_tag(),
                "confidence": a.confidence,
                "reason": a.reason,
                "source": "ai",
            });
            ops.push(DiffOperation::Update { target, patch });
        }
        Diff {
            id: DiffId::new(),
            tool: ToolName::Classification,
            status: DiffStatus::Pending,
            operations: ops,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClassificationConfig {
    /// Minimum confidence to emit a proposal at all. Defaults to 0.5
    /// (anything below isn't worth showing to the user); the BIM
    /// store's own threshold (default 0.85) gates whether the
    /// proposal is automatically accepted.
    pub min_confidence: f64,
}

impl Default for ClassificationConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
        }
    }
}

pub fn classify(
    features: &[GeometryFeatures],
    config: &ClassificationConfig,
) -> ClassificationResult {
    let mut assignments = Vec::with_capacity(features.len());
    for f in features {
        let (class, conf, reason) = classify_one(f);
        if conf < config.min_confidence {
            continue;
        }
        assignments.push(ClassificationProposal {
            entity: f.entity.clone(),
            ifc_class: class,
            confidence: conf,
            reason,
        });
    }
    ClassificationResult { assignments }
}

fn classify_one(f: &GeometryFeatures) -> (IfcClass, f64, String) {
    let l = f.longest_horizontal();
    let w = f.shortest_horizontal();
    let h = f.height();
    let floor_clearance = f.centroid[2] - h * 0.5;

    // Door: thickness (w) < 0.3 m, leaf width (l) 0.6–1.5 m,
    // height 1.8–2.4 m, sits at floor level, and either the parent
    // is a wall or the mesh has openings cut through it.
    if w < 0.3
        && (0.6..=1.5).contains(&l)
        && (1.8..=2.4).contains(&h)
        && floor_clearance.abs() < 0.3
        && (f.parent_class.as_deref() == Some("IfcWall") || f.has_openings)
    {
        return (
            IfcClass::IfcDoor,
            0.92,
            format!("door-like box {:.2}×{:.2}×{:.2} m at floor", l, w, h),
        );
    }

    // Window: thickness (w) < 0.3 m, leaf width (l) 0.4–3.0 m,
    // height 0.4–2.2 m, sits 0.4–1.5 m above floor.
    if w < 0.3
        && (0.4..=2.2).contains(&h)
        && (0.4..=3.0).contains(&l)
        && (0.4..=1.5).contains(&floor_clearance)
    {
        return (
            IfcClass::IfcWindow,
            0.90,
            format!(
                "window-like box {:.2}×{:.2} m at sill height {:.2} m",
                l, h, floor_clearance
            ),
        );
    }

    // Column: tall + small footprint area + roughly square footprint.
    let footprint = f.footprint_area();
    if footprint < 1.0 && h > 2.0 && f.aspect_length_over_width() < 1.5 {
        return (
            IfcClass::IfcColumn,
            0.88,
            format!("column: footprint {:.2} m², height {:.2} m", footprint, h),
        );
    }

    // Beam: long, thin, horizontal, elevated above floor.
    if l > 1.5 && w < 0.6 && h < 0.8 && floor_clearance > 0.5 {
        return (
            IfcClass::IfcBeam,
            0.86,
            format!(
                "beam: {:.2} m long, {:.2}×{:.2} m section, sitting {:.2} m above floor",
                l, w, h, floor_clearance
            ),
        );
    }

    // Wall: long, thin, tall.
    if l > 0.8 && w < 0.6 && h > 1.5 {
        return (
            IfcClass::IfcWall,
            0.93,
            format!("wall: {:.2}×{:.2}×{:.2} m", l, w, h),
        );
    }

    // Slab: thin horizontal extent with large XY area.
    if h < 0.6 && footprint > 1.0 && l > 1.0 && w > 0.5 {
        let class = if floor_clearance > 2.5 {
            IfcClass::IfcRoof
        } else {
            IfcClass::IfcSlab
        };
        return (
            class,
            0.90,
            format!(
                "{}: {:.2} m² × {:.2} m thick at z={:.2} m",
                if floor_clearance > 2.5 {
                    "roof"
                } else {
                    "slab"
                },
                footprint,
                h,
                floor_clearance
            ),
        );
    }

    // Catch-all
    (
        IfcClass::Other("IfcBuildingElementProxy".into()),
        0.55,
        "no rule matched — falling back to IfcBuildingElementProxy".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(
        entity: &str,
        sx: f64,
        sy: f64,
        sz: f64,
        cx: f64,
        cy: f64,
        cz: f64,
    ) -> GeometryFeatures {
        GeometryFeatures {
            entity: entity.into(),
            size: [sx, sy, sz],
            centroid: [cx, cy, cz],
            parent_class: None,
            has_openings: false,
        }
    }

    #[test]
    fn classifies_a_wall() {
        let f = feat("w1", 5.0, 0.2, 3.0, 0.0, 0.0, 1.5);
        let (c, conf, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcWall);
        assert!(conf > 0.85);
    }

    #[test]
    fn classifies_a_slab() {
        let f = feat("s1", 8.0, 6.0, 0.2, 0.0, 0.0, 0.1);
        let (c, conf, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcSlab);
        assert!(conf > 0.85);
    }

    #[test]
    fn classifies_a_roof_when_elevated() {
        let f = feat("r1", 8.0, 6.0, 0.2, 0.0, 0.0, 6.0);
        let (c, _, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcRoof);
    }

    #[test]
    fn classifies_a_column() {
        let f = feat("c1", 0.5, 0.5, 3.0, 0.0, 0.0, 1.5);
        let (c, conf, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcColumn);
        assert!(conf > 0.85);
    }

    #[test]
    fn classifies_a_beam() {
        let f = feat("b1", 5.0, 0.3, 0.5, 0.0, 0.0, 3.0);
        let (c, _, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcBeam);
    }

    #[test]
    fn classifies_a_door_when_parent_is_wall() {
        let mut f = feat("d1", 0.1, 0.9, 2.1, 0.0, 0.0, 1.05);
        f.parent_class = Some("IfcWall".into());
        let (c, conf, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcDoor);
        assert!(conf > 0.85);
    }

    #[test]
    fn classifies_a_window_at_sill_height() {
        let f = feat("win", 1.5, 0.1, 1.2, 0.0, 0.0, 1.5);
        let (c, _, _) = classify_one(&f);
        assert_eq!(c, IfcClass::IfcWindow);
    }

    #[test]
    fn unknown_shape_falls_back_to_proxy_below_threshold() {
        let f = feat("blob", 0.4, 0.4, 0.4, 0.0, 0.0, 1.0);
        let (c, conf, _) = classify_one(&f);
        assert_eq!(c, IfcClass::Other("IfcBuildingElementProxy".into()));
        assert!(conf < 0.85);
    }

    #[test]
    fn classify_filters_below_min_confidence() {
        let cfg = ClassificationConfig {
            min_confidence: 0.6,
        };
        let blob = feat("blob", 0.4, 0.4, 0.4, 0.0, 0.0, 1.0);
        let r = classify(&[blob], &cfg);
        assert!(r.assignments.is_empty());
    }

    #[test]
    fn diff_is_one_op_per_assignment() {
        let cfg = ClassificationConfig::default();
        let mut wall = feat("ignored", 5.0, 0.2, 3.0, 0.0, 0.0, 1.5);
        let mut slab = feat("ignored2", 8.0, 6.0, 0.2, 0.0, 0.0, 0.1);
        wall.entity = EntityId::new().to_string();
        slab.entity = EntityId::new().to_string();
        let r = classify(&[wall, slab], &cfg);
        let d = r.to_diff();
        assert_eq!(d.operations.len(), 2);
        for op in &d.operations {
            match op {
                DiffOperation::Update { patch, .. } => {
                    assert!(patch.get("ifc_class").is_some());
                    assert_eq!(patch.get("source").and_then(|v| v.as_str()), Some("ai"));
                }
                _ => panic!("expected Update op"),
            }
        }
    }

    #[test]
    fn diff_skips_proposals_with_invalid_entity_ids() {
        let cfg = ClassificationConfig::default();
        let wall = feat("not_an_entity_id", 5.0, 0.2, 3.0, 0.0, 0.0, 1.5);
        let r = classify(&[wall], &cfg);
        let d = r.to_diff();
        assert!(d.operations.is_empty());
    }

    #[test]
    fn classifier_output_is_grammar_valid() {
        let cfg = ClassificationConfig::default();
        let wall = feat("w1", 5.0, 0.2, 3.0, 0.0, 0.0, 1.5);
        let r = classify(&[wall], &cfg);
        let payload = serde_json::to_string(&serde_json::json!({
            "assignments": r.assignments.iter().map(|a| serde_json::json!({
                "entity": a.entity,
                "ifc_class": a.ifc_class.ifc_tag(),
                "confidence": a.confidence,
            })).collect::<Vec<_>>(),
        }))
        .unwrap();
        let g = crate::grammars::GrammarRegistry::defaults();
        let grammar = g.get("classification").unwrap();
        assert!(grammar.matches(&payload), "{}", payload);
    }
}
