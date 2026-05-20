//! Layout suggestion — typed wrapper around the `layout_suggestion`
//! tool response.
//!
//! Where [`crate::style_assistant::StyleAssistantResult`] proposes *what*
//! to put in a room (furniture asset ids, materials, mood), the layout
//! suggestion tool proposes *where* to put it: a list of furniture
//! [`LayoutProposal`]s with explicit positions, rotations, and a target
//! room anchor.
//!
//! The grammar (`layout_suggestion` in [`crate::grammars`]) constrains
//! the sidecar to emit exactly this shape; the diff engine
//! ([`crate::diff_engine`]) turns these proposals into `Insert` /
//! `Update` operations against the project entity store.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::planner::PlanResponse;

/// One placement instruction: which asset goes where, in what
/// orientation. If `target_entity` is set the proposal updates an
/// existing furniture entity; otherwise it inserts a new one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutProposal {
    /// Asset catalogue id (matches `aec_assets::AssetSummary::id`). Required
    /// for inserts; optional for updates that only reposition.
    #[serde(default)]
    pub asset_id: Option<String>,
    /// Optional existing entity id; when present the planner emits an
    /// `Update` to reposition this entity rather than inserting a new one.
    #[serde(default)]
    pub target_entity: Option<EntityId>,
    /// Position in millimetres relative to the room anchor (x, y, z).
    pub position_mm: [f64; 3],
    /// Z-axis rotation in degrees (planar yaw — what a top-down
    /// floorplan editor cares about).
    pub rotation_deg: f64,
}

impl LayoutProposal {
    /// True if this proposal repositions an existing entity (an Update
    /// in diff-engine terms) rather than introducing a new one.
    pub fn is_update(&self) -> bool {
        self.target_entity.is_some()
    }

    /// Validate the proposal is well-formed: finite numerics and either
    /// an asset id (for inserts) or a target entity (for updates).
    pub fn validate(&self) -> Result<(), LayoutValidationError> {
        if !self.position_mm.iter().all(|c| c.is_finite()) {
            return Err(LayoutValidationError::NonFinitePosition);
        }
        if !self.rotation_deg.is_finite() {
            return Err(LayoutValidationError::NonFiniteRotation);
        }
        if self.asset_id.is_none() && self.target_entity.is_none() {
            return Err(LayoutValidationError::NoTarget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutValidationError {
    NonFinitePosition,
    NonFiniteRotation,
    NoTarget,
}

impl std::fmt::Display for LayoutValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinitePosition => f.write_str("position must be finite"),
            Self::NonFiniteRotation => f.write_str("rotation must be finite"),
            Self::NoTarget => f.write_str("proposal needs an asset_id or target_entity"),
        }
    }
}

impl std::error::Error for LayoutValidationError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSuggestionResult {
    /// Identifier of the room (a space / zone entity) this layout was
    /// proposed for — used as the spatial anchor for `position_mm`.
    pub room_anchor: EntityId,
    /// One entry per piece of furniture in the proposal.
    pub proposals: Vec<LayoutProposal>,
}

impl LayoutSuggestionResult {
    /// Decode a [`PlanResponse`] produced by the `layout_suggestion`
    /// tool. Returns `None` if the payload doesn't carry the expected
    /// fields or contains non-finite numbers.
    pub fn from_response(r: &PlanResponse) -> Option<Self> {
        let room_str = r.parsed.get("room_anchor").and_then(|v| v.as_str())?;
        let room_anchor = EntityId::from_string(room_str).ok()?;
        let arr = r.parsed.get("proposals").and_then(|v| v.as_array())?;
        let mut proposals = Vec::with_capacity(arr.len());
        for entry in arr {
            let p = parse_proposal(entry)?;
            // Reject malformed proposals up-front — the diff engine
            // would have to defend against them anyway, and surfacing
            // it here means the planner sees a clean typed value.
            p.validate().ok()?;
            proposals.push(p);
        }
        Some(Self {
            room_anchor,
            proposals,
        })
    }
}

fn parse_proposal(v: &serde_json::Value) -> Option<LayoutProposal> {
    let pos = v.get("position_mm")?.as_array()?;
    if pos.len() != 3 {
        return None;
    }
    let mut position = [0.0f64; 3];
    for (i, slot) in position.iter_mut().enumerate() {
        *slot = pos.get(i)?.as_f64()?;
    }
    let rotation_deg = v
        .get("rotation_deg")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let asset_id = v.get("asset_id").and_then(|x| x.as_str()).map(String::from);
    let target_entity = v
        .get("target_entity")
        .and_then(|x| x.as_str())
        .and_then(|s| EntityId::from_string(s).ok());
    Some(LayoutProposal {
        asset_id,
        target_entity,
        position_mm: position,
        rotation_deg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schema::ToolName;

    fn response(payload: serde_json::Value) -> PlanResponse {
        PlanResponse {
            tool: ToolName::LayoutSuggestion,
            raw_payload: payload.to_string(),
            parsed: payload,
            entities_modified: 3,
        }
    }

    #[test]
    fn parses_full_payload_with_inserts_and_updates() {
        let anchor = EntityId::new();
        let target = EntityId::new();
        let r = response(serde_json::json!({
            "room_anchor": anchor.as_str(),
            "proposals": [
                {
                    "asset_id": "ast:sofa_a",
                    "position_mm": [1200.0, 800.0, 0.0],
                    "rotation_deg": 90.0,
                },
                {
                    "target_entity": target.as_str(),
                    "position_mm": [2400.0, 1600.0, 0.0],
                    "rotation_deg": -45.0,
                },
            ],
        }));
        let res = LayoutSuggestionResult::from_response(&r).expect("valid payload");
        assert_eq!(res.room_anchor, anchor);
        assert_eq!(res.proposals.len(), 2);
        assert!(!res.proposals[0].is_update());
        assert!(res.proposals[1].is_update());
        assert_eq!(res.proposals[1].target_entity.as_ref(), Some(&target));
    }

    #[test]
    fn rejects_payload_with_non_finite_numbers() {
        let anchor = EntityId::new();
        let r = response(serde_json::json!({
            "room_anchor": anchor.as_str(),
            "proposals": [{
                "asset_id": "ast:chair",
                "position_mm": [f64::NAN, 0.0, 0.0],
                "rotation_deg": 0.0,
            }],
        }));
        assert!(LayoutSuggestionResult::from_response(&r).is_none());
    }

    #[test]
    fn rejects_proposal_without_asset_or_target() {
        let anchor = EntityId::new();
        let r = response(serde_json::json!({
            "room_anchor": anchor.as_str(),
            "proposals": [{
                "position_mm": [0.0, 0.0, 0.0],
                "rotation_deg": 0.0,
            }],
        }));
        assert!(LayoutSuggestionResult::from_response(&r).is_none());
    }

    #[test]
    fn rejects_missing_room_anchor() {
        let r = response(serde_json::json!({
            "proposals": [],
        }));
        assert!(LayoutSuggestionResult::from_response(&r).is_none());
    }

    #[test]
    fn validates_individual_proposals() {
        let p = LayoutProposal {
            asset_id: Some("ast:foo".into()),
            target_entity: None,
            position_mm: [0.0, 0.0, 0.0],
            rotation_deg: 0.0,
        };
        assert!(p.validate().is_ok());
        let bad = LayoutProposal {
            asset_id: None,
            target_entity: None,
            position_mm: [0.0, 0.0, 0.0],
            rotation_deg: 0.0,
        };
        assert_eq!(bad.validate().unwrap_err(), LayoutValidationError::NoTarget);
    }
}
