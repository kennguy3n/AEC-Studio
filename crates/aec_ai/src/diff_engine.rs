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
            ToolName::StyleAssistant | ToolName::LayoutSuggestion => {
                build_style_assistant(response)
            }
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
}
