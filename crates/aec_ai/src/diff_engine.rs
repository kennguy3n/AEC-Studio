//! Diff engine. Turns a [`PlanResponse`] into a list of previewable
//! [`DiffOperation`]s the command engine can apply, reject, or persist.
//!
//! `Diff` is opaque to the AI — the AI only produces tool-call payloads.
//! The diff engine is the bridge: it interprets each tool's payload into
//! commands.

use serde::{Deserialize, Serialize};

use aec_core::types::{DiffId, EntityId};

use crate::planner::PlanResponse;
use crate::tool_schema::ToolName;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffOperation {
    Insert {
        entity_kind: String,
        payload: serde_json::Value,
    },
    Update {
        target: EntityId,
        patch: serde_json::Value,
    },
    Delete {
        target: EntityId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    pub id: DiffId,
    pub tool: ToolName,
    pub status: DiffStatus,
    pub operations: Vec<DiffOperation>,
}

pub struct DiffEngine;

impl DiffEngine {
    /// Convert a [`PlanResponse`] into a [`Diff`].
    ///
    /// The interpretation is per-tool:
    ///   - `PlanDetection` / `PlanToWall`: each polyline becomes an Insert
    ///     for an `aec_geometry::Wall`.
    ///   - `StyleAssistant`: furniture_ids → Inserts of furniture; one
    ///     Update for the lighting preset; one Update per material binding.
    ///   - `RenderDoctor`: produces Update operations against the active
    ///     render preset entity.
    pub fn build(response: &PlanResponse) -> Diff {
        let id = DiffId::new();
        let operations = match response.tool {
            ToolName::PlanDetection | ToolName::PlanToWall => build_plan_detection(response),
            ToolName::StyleAssistant => build_style_assistant(response),
            ToolName::LayoutSuggestion => build_layout_suggestion(response),
            ToolName::RenderDoctor => build_render_doctor(response),
            _ => Vec::new(),
        };
        Diff {
            id,
            tool: response.tool,
            status: DiffStatus::Pending,
            operations,
        }
    }
}

fn build_plan_detection(response: &PlanResponse) -> Vec<DiffOperation> {
    let mut out = Vec::new();
    if let Some(arr) = response.parsed.get("polylines").and_then(|v| v.as_array()) {
        for poly in arr {
            if poly.get("points").is_some() {
                out.push(DiffOperation::Insert {
                    entity_kind: "wall".into(),
                    payload: poly.clone(),
                });
            }
        }
    }
    out
}

fn build_style_assistant(response: &PlanResponse) -> Vec<DiffOperation> {
    let mut out = Vec::new();
    if let Some(arr) = response
        .parsed
        .get("furniture_ids")
        .and_then(|v| v.as_array())
    {
        for asset in arr {
            if let Some(asset_id) = asset.as_str() {
                out.push(DiffOperation::Insert {
                    entity_kind: "furniture".into(),
                    payload: serde_json::json!({ "asset_id": asset_id }),
                });
            }
        }
    }
    if let Some(arr) = response
        .parsed
        .get("material_ids")
        .and_then(|v| v.as_array())
    {
        for mat in arr {
            if let Some(material_id) = mat.as_str() {
                out.push(DiffOperation::Insert {
                    entity_kind: "material_binding".into(),
                    payload: serde_json::json!({ "material_id": material_id }),
                });
            }
        }
    }
    if let Some(preset) = response
        .parsed
        .get("lighting_preset_id")
        .and_then(|v| v.as_str())
    {
        out.push(DiffOperation::Insert {
            entity_kind: "lighting_preset".into(),
            payload: serde_json::json!({ "preset_id": preset }),
        });
    }
    out
}

/// Convert a `layout_suggestion` payload into a mix of `Insert`
/// (new furniture placement) and `Update` (repositioning an existing
/// piece in place) diff operations.
///
/// The tool may carry either an `asset_id` (insert) or a
/// `target_entity` (update) per proposal. The `room_anchor` is
/// propagated into every operation so the command engine can resolve
/// positions relative to the correct space.
fn build_layout_suggestion(response: &PlanResponse) -> Vec<DiffOperation> {
    let mut out = Vec::new();
    let anchor = response
        .parsed
        .get("room_anchor")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let Some(arr) = response.parsed.get("proposals").and_then(|v| v.as_array()) else {
        return out;
    };
    for proposal in arr {
        let pos = proposal.get("position_mm").cloned().unwrap_or_default();
        let rot = proposal.get("rotation_deg").cloned().unwrap_or_default();
        let target = proposal
            .get("target_entity")
            .and_then(|v| v.as_str())
            .and_then(|s| EntityId::from_string(s).ok());
        if let Some(target) = target {
            // Repositioning existing furniture: emit Update.
            out.push(DiffOperation::Update {
                target,
                patch: serde_json::json!({
                    "position_mm": pos,
                    "rotation_deg": rot,
                    "room_anchor": anchor,
                }),
            });
        } else if let Some(asset_id) = proposal.get("asset_id").and_then(|v| v.as_str()) {
            // New placement: emit Insert with the full payload.
            out.push(DiffOperation::Insert {
                entity_kind: "furniture".into(),
                payload: serde_json::json!({
                    "asset_id": asset_id,
                    "position_mm": pos,
                    "rotation_deg": rot,
                    "room_anchor": anchor,
                }),
            });
        }
    }
    out
}

fn build_render_doctor(response: &PlanResponse) -> Vec<DiffOperation> {
    let mut out = Vec::new();
    if let Some(arr) = response.parsed.get("findings").and_then(|v| v.as_array()) {
        for finding in arr {
            out.push(DiffOperation::Update {
                target: EntityId::from_string("ent_render_preset_active")
                    .unwrap_or_else(|_| EntityId::new()),
                patch: finding.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_response(tool: ToolName, payload: serde_json::Value) -> PlanResponse {
        PlanResponse {
            tool,
            raw_payload: payload.to_string(),
            parsed: payload,
            entities_modified: 1,
        }
    }

    #[test]
    fn plan_detection_yields_wall_inserts() {
        let p = plan_response(
            ToolName::PlanDetection,
            serde_json::json!({
                "polylines": [
                    { "points": [[0,0],[3000,0]] },
                    { "points": [[3000,0],[3000,2400]] }
                ]
            }),
        );
        let diff = DiffEngine::build(&p);
        assert_eq!(diff.operations.len(), 2);
        assert!(matches!(diff.operations[0], DiffOperation::Insert { .. }));
    }

    #[test]
    fn style_assistant_yields_furniture_material_lighting() {
        let p = plan_response(
            ToolName::StyleAssistant,
            serde_json::json!({
                "furniture_ids": ["ast:sofa"],
                "material_ids": ["mat:oak"],
                "lighting_preset_id": "warm_evening",
            }),
        );
        let diff = DiffEngine::build(&p);
        assert_eq!(diff.operations.len(), 3);
    }

    #[test]
    fn layout_suggestion_emits_inserts_for_new_furniture() {
        let p = plan_response(
            ToolName::LayoutSuggestion,
            serde_json::json!({
                "room_anchor": "ent_living_room",
                "proposals": [
                    {
                        "asset_id": "ast:sofa_a",
                        "position_mm": [1200.0, 800.0, 0.0],
                        "rotation_deg": 90.0,
                    },
                    {
                        "asset_id": "ast:chair_b",
                        "position_mm": [2400.0, 1600.0, 0.0],
                        "rotation_deg": 0.0,
                    },
                ],
            }),
        );
        let diff = DiffEngine::build(&p);
        assert_eq!(diff.tool, ToolName::LayoutSuggestion);
        assert_eq!(diff.operations.len(), 2);
        for op in &diff.operations {
            match op {
                DiffOperation::Insert {
                    entity_kind,
                    payload,
                } => {
                    assert_eq!(entity_kind, "furniture");
                    assert!(payload.get("asset_id").is_some());
                    assert!(payload.get("position_mm").is_some());
                    assert_eq!(
                        payload.get("room_anchor").and_then(|v| v.as_str()),
                        Some("ent_living_room")
                    );
                }
                other => panic!("expected Insert, got {other:?}"),
            }
        }
    }

    #[test]
    fn layout_suggestion_emits_updates_for_existing_furniture() {
        let target = EntityId::new();
        let p = plan_response(
            ToolName::LayoutSuggestion,
            serde_json::json!({
                "room_anchor": "ent_living_room",
                "proposals": [
                    {
                        "target_entity": target.as_str(),
                        "position_mm": [2000.0, 1200.0, 0.0],
                        "rotation_deg": -45.0,
                    },
                ],
            }),
        );
        let diff = DiffEngine::build(&p);
        assert_eq!(diff.operations.len(), 1);
        match &diff.operations[0] {
            DiffOperation::Update { target: t, patch } => {
                assert_eq!(*t, target);
                assert!(patch.get("position_mm").is_some());
                assert!(patch.get("rotation_deg").is_some());
                assert_eq!(
                    patch.get("room_anchor").and_then(|v| v.as_str()),
                    Some("ent_living_room")
                );
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn layout_suggestion_mixed_inserts_and_updates() {
        let existing = EntityId::new();
        let p = plan_response(
            ToolName::LayoutSuggestion,
            serde_json::json!({
                "room_anchor": "ent_room",
                "proposals": [
                    {
                        "asset_id": "ast:chair",
                        "position_mm": [0.0, 0.0, 0.0],
                        "rotation_deg": 0.0,
                    },
                    {
                        "target_entity": existing.as_str(),
                        "position_mm": [500.0, 0.0, 0.0],
                        "rotation_deg": 180.0,
                    },
                ],
            }),
        );
        let diff = DiffEngine::build(&p);
        let inserts = diff
            .operations
            .iter()
            .filter(|o| matches!(o, DiffOperation::Insert { .. }))
            .count();
        let updates = diff
            .operations
            .iter()
            .filter(|o| matches!(o, DiffOperation::Update { .. }))
            .count();
        assert_eq!(inserts, 1, "one insert for new furniture");
        assert_eq!(updates, 1, "one update for repositioned furniture");
    }
}
