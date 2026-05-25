//! Convert an AI [`Diff`] into a sequence of typed [`Command`]s that the
//! command engine can apply, journal, and undo.
//!
//! Phase 11 task 10 — "AI accept-diff → command-engine apply" lives here.
//! `BridgeService::ai_accept_diff` previously discarded the accepted diff
//! after recording the acceptance; the actual mutation never reached the
//! project graph. This module is the real apply path: it takes a
//! [`Diff`] (which the diff engine already produced from a parsed
//! [`aec_ai::PlanResponse`]) and turns each [`DiffOperation`] into a
//! [`Command`] tagged with [`aec_core::types::ActorKind::Ai`] so the
//! audit chain attributes the mutation to the model.
//!
//! ## Mapping table
//!
//! | `DiffOperation` shape                                | Emitted `CommandKind`                |
//! |------------------------------------------------------|--------------------------------------|
//! | `Insert { entity_kind: "wall", payload }`            | one `CreateWall` per polyline segment |
//! | `Insert { entity_kind: "furniture", payload }`       | `PlaceFurniture`                     |
//! | `Insert { entity_kind: "lighting_preset", payload }` | `SetLighting`                        |
//! | `Insert { entity_kind: "material_binding", payload }`| `PaintMaterial` *iff* target supplied|
//! | `Update { target, patch }` for `kind=furniture`      | `MoveFurniture`                      |
//! | `Update { target, patch }` for `kind=camera`         | `UpdateCamera`                       |
//! | `Delete { target }` for `kind=furniture`             | `DeleteFurniture`                    |
//! | `Delete { target }` for `kind=wall`                  | `DeleteWall`                         |
//! | `Delete { target }` for `kind=camera`                | `DeleteCamera`                       |
//! | `Delete { target }` for `kind=light`                 | `RemoveLight`                        |
//!
//! Operations that cannot be applied (render_doctor diagnostics that
//! target the synthetic `ent_render_preset_active` sentinel, material
//! bindings missing a `target_entity`, update/delete operations on
//! entity kinds that don't have a typed command) are returned via
//! [`ApplyConversion::skipped`] rather than silently dropped, so the
//! audit log can record what the AI tried to do even when the apply
//! is a no-op. Future iterations can extend the conversion to cover
//! more entity types — the contract here is intentionally permissive
//! ("apply what we can, surface the rest") rather than strict ("all
//! or nothing"), so a multi-op AI plan with one unknown operation
//! still produces value for the remaining operations.
//!
//! ## Defaults
//!
//! Wall and furniture commands carry quantitative fields that the AI
//! diff payload does not currently provide (wall height + thickness;
//! furniture position is allowed to be absent on a pure asset-shelf
//! diff). [`ApplyDefaults`] is the single source of truth for those
//! defaults; the service layer reads project-level settings (where
//! available) to populate it.

use serde_json::Value;

use aec_core::types::EntityId;

use aec_ai::{Diff, DiffOperation, ToolName};

use crate::commands::{
    camera::{CameraParams, DeleteCamera, UpdateCamera},
    furniture::{is_furniture_kind, DeleteFurniture, MoveFurniture, PlaceFurniture},
    lighting::{RemoveLight, SetLighting},
    material::PaintMaterial,
    wall::{CreateWall, DeleteWall},
    Command, CommandKind, ProjectGraph,
};

/// Project-level defaults used when the AI diff payload doesn't carry
/// every field needed to build a typed command. The service layer
/// populates this from the project's manifest / first room / template
/// before invoking [`diff_to_commands`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApplyDefaults {
    /// Default wall height in millimetres. Used when the AI diff
    /// payload omits a height (every current `plan_detection` /
    /// `plan_to_wall` payload does — the model only emits 2D
    /// polylines). 2400 mm is the canonical interior ceiling height
    /// for residential templates; the service can override for
    /// commercial templates.
    pub wall_height_mm: f64,
    /// Default wall thickness in millimetres. Mirrors the
    /// `default_walls.interior_thickness_mm` field from the template
    /// JSON; 100 mm is the residential interior default.
    pub wall_thickness_mm: f64,
}

impl Default for ApplyDefaults {
    fn default() -> Self {
        Self {
            wall_height_mm: 2400.0,
            wall_thickness_mm: 100.0,
        }
    }
}

/// One operation that the converter could not translate into a typed
/// command, along with a short human-readable reason. The service
/// layer surfaces these alongside the applied count so the renderer
/// (and the AI audit trail) can show "applied 4 of 5 — 1 skipped:
/// material binding missing target".
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedOperation {
    pub op_index: usize,
    pub reason: String,
}

/// Result of [`diff_to_commands`]: the typed commands ready for
/// [`crate::engine::CommandEngine::execute_persistent_batch`], plus
/// the per-operation skip reasons.
#[derive(Debug, Clone)]
pub struct ApplyConversion {
    pub commands: Vec<Command>,
    pub skipped: Vec<SkippedOperation>,
}

impl ApplyConversion {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Convert every [`DiffOperation`] in `diff` into one or more typed
/// [`Command`]s. Each emitted command is attributed to the AI tool
/// that produced the diff (`Command::ai(tool_name, kind)`).
///
/// The conversion is **fail-soft**: an operation that cannot be
/// mapped (unknown entity kind, missing required payload field,
/// dangling `target` id) is recorded in `skipped` rather than
/// returned as an error. This matches the renderer's contract — the
/// user already reviewed the diff in the AI panel and clicked
/// Accept, so the service layer should commit whatever subset of the
/// operations the schema understands rather than refusing the whole
/// accept and leaving the user with no way to make progress.
pub fn diff_to_commands(
    diff: &Diff,
    graph: &ProjectGraph,
    defaults: ApplyDefaults,
) -> ApplyConversion {
    let tool = diff.tool.as_str().to_owned();
    let mut commands: Vec<Command> = Vec::new();
    let mut skipped: Vec<SkippedOperation> = Vec::new();

    for (op_index, op) in diff.operations.iter().enumerate() {
        match op {
            DiffOperation::Insert {
                entity_kind,
                payload,
            } => convert_insert(
                op_index,
                entity_kind,
                payload,
                &tool,
                defaults,
                diff.tool,
                &mut commands,
                &mut skipped,
            ),
            DiffOperation::Update { target, patch } => convert_update(
                op_index,
                target,
                patch,
                &tool,
                graph,
                &mut commands,
                &mut skipped,
            ),
            DiffOperation::Delete { target } => {
                convert_delete(op_index, target, &tool, graph, &mut commands, &mut skipped);
            }
        }
    }

    ApplyConversion { commands, skipped }
}

#[allow(clippy::too_many_arguments)]
fn convert_insert(
    op_index: usize,
    entity_kind: &str,
    payload: &Value,
    tool: &str,
    defaults: ApplyDefaults,
    diff_tool: ToolName,
    commands: &mut Vec<Command>,
    skipped: &mut Vec<SkippedOperation>,
) {
    match entity_kind {
        "wall" => insert_walls_from_polyline(op_index, payload, tool, defaults, commands, skipped),
        "furniture" => match build_place_furniture(payload) {
            Ok(cmd) => commands.push(Command::ai(tool, CommandKind::PlaceFurniture(cmd))),
            Err(reason) => skipped.push(SkippedOperation { op_index, reason }),
        },
        "lighting_preset" => match build_set_lighting(payload) {
            Ok(cmd) => commands.push(Command::ai(tool, CommandKind::SetLighting(cmd))),
            Err(reason) => skipped.push(SkippedOperation { op_index, reason }),
        },
        "material_binding" => match build_paint_material(payload) {
            Ok(cmd) => commands.push(Command::ai(tool, CommandKind::PaintMaterial(cmd))),
            Err(reason) => skipped.push(SkippedOperation { op_index, reason }),
        },
        // `render_doctor` emits Update operations against
        // `ent_render_preset_active`; the diff engine doesn't emit
        // Insert ops for render diagnostics today, but if a future
        // tool starts to we'd land here. Skip with an explicit
        // reason so the audit trail still captures the attempt.
        other if diff_tool == ToolName::RenderDoctor => {
            skipped.push(SkippedOperation {
                op_index,
                reason: format!(
                    "render_doctor insert of `{other}` is informational; not applied to graph",
                ),
            });
        }
        other => skipped.push(SkippedOperation {
            op_index,
            reason: format!("unsupported insert entity_kind `{other}`"),
        }),
    }
}

fn insert_walls_from_polyline(
    op_index: usize,
    payload: &Value,
    tool: &str,
    defaults: ApplyDefaults,
    commands: &mut Vec<Command>,
    skipped: &mut Vec<SkippedOperation>,
) {
    let Some(points) = payload.get("points").and_then(|v| v.as_array()) else {
        skipped.push(SkippedOperation {
            op_index,
            reason: "wall insert missing `points` array".into(),
        });
        return;
    };
    if points.len() < 2 {
        skipped.push(SkippedOperation {
            op_index,
            reason: format!(
                "wall insert needs >= 2 points to form a segment, got {}",
                points.len()
            ),
        });
        return;
    }
    let parsed: Option<Vec<[f64; 2]>> = points
        .iter()
        .map(|pt| {
            let arr = pt.as_array()?;
            if arr.len() < 2 {
                return None;
            }
            Some([arr[0].as_f64()?, arr[1].as_f64()?])
        })
        .collect();
    let Some(pts) = parsed else {
        skipped.push(SkippedOperation {
            op_index,
            reason: "wall insert `points` entries must be [x, y] number pairs".into(),
        });
        return;
    };
    for pair in pts.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if start == end {
            // Collapsed segment; `CreateWall::validate` would reject
            // it. Skip silently so a polyline with a duplicate point
            // (common from autotraced floorplans) doesn't fail the
            // whole batch.
            skipped.push(SkippedOperation {
                op_index,
                reason: "zero-length wall segment (start == end)".into(),
            });
            continue;
        }
        let cmd = CreateWall {
            entity_id: EntityId::new(),
            start_mm: start,
            end_mm: end,
            height_mm: defaults.wall_height_mm,
            thickness_mm: defaults.wall_thickness_mm,
            material_id: None,
        };
        commands.push(Command::ai(tool, CommandKind::CreateWall(cmd)));
    }
}

fn build_place_furniture(payload: &Value) -> Result<PlaceFurniture, String> {
    let asset_id = payload
        .get("asset_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "furniture insert missing `asset_id`".to_string())?;
    let position_mm = parse_xyz(payload.get("position_mm"))?;
    let rotation_yaw_deg = payload
        .get("rotation_deg")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let parent = payload
        .get("room_anchor")
        .and_then(|v| v.as_str())
        .and_then(|s| EntityId::from_string(s.to_string()).ok());
    Ok(PlaceFurniture {
        entity_id: EntityId::new(),
        asset_ref: asset_id.to_owned(),
        position_mm,
        rotation_yaw_deg,
        scale_override: None,
        name: None,
        parent,
    })
}

fn build_set_lighting(payload: &Value) -> Result<SetLighting, String> {
    let preset_id = payload
        .get("preset_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "lighting_preset insert missing `preset_id`".to_string())?;
    Ok(SetLighting {
        preset_id: preset_id.to_owned(),
    })
}

fn build_paint_material(payload: &Value) -> Result<PaintMaterial, String> {
    let material_id = payload
        .get("material_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "material_binding insert missing `material_id`".to_string())?;
    // The diff engine's `style_assistant` payload does not yet carry
    // a target entity for material bindings — the model returns a
    // bare `material_ids: [...]` list. Skipping these here is the
    // current best behaviour; once the planner contract is extended
    // to bind a material to a target entity, this branch starts
    // emitting real `PaintMaterial` commands.
    let target = payload
        .get("target_entity")
        .or_else(|| payload.get("target_entity_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            "material_binding insert missing `target_entity` (no entity to paint)".to_string()
        })?;
    let target_entity_id = EntityId::from_string(target.to_string())
        .map_err(|e| format!("material_binding `target_entity` is not a valid id: {e}"))?;
    let surface = payload
        .get("surface")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    Ok(PaintMaterial {
        target_entity_id,
        material_id: material_id.to_owned(),
        surface,
    })
}

fn convert_update(
    op_index: usize,
    target: &EntityId,
    patch: &Value,
    tool: &str,
    graph: &ProjectGraph,
    commands: &mut Vec<Command>,
    skipped: &mut Vec<SkippedOperation>,
) {
    let Some(record) = graph.get(target) else {
        skipped.push(SkippedOperation {
            op_index,
            reason: format!("update target `{target}` not found in project graph"),
        });
        return;
    };
    let kind = record.kind.as_str();
    if is_furniture_kind(kind) {
        match build_move_furniture(target.clone(), patch) {
            Ok(cmd) => commands.push(Command::ai(tool, CommandKind::MoveFurniture(cmd))),
            Err(reason) => skipped.push(SkippedOperation { op_index, reason }),
        }
        return;
    }
    if kind == "camera" {
        match build_update_camera(target.clone(), patch, record) {
            Ok(cmd) => commands.push(Command::ai(tool, CommandKind::UpdateCamera(cmd))),
            Err(reason) => skipped.push(SkippedOperation { op_index, reason }),
        }
        return;
    }
    skipped.push(SkippedOperation {
        op_index,
        reason: format!("update for entity kind `{kind}` not yet supported"),
    });
}

fn build_move_furniture(target: EntityId, patch: &Value) -> Result<MoveFurniture, String> {
    let position_mm = parse_xyz(patch.get("position_mm"))?;
    let rotation_yaw_deg = patch
        .get("rotation_deg")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    Ok(MoveFurniture {
        entity_id: target,
        position_mm,
        rotation_yaw_deg,
        scale_override: None,
    })
}

fn build_update_camera(
    target: EntityId,
    patch: &Value,
    record: &crate::commands::EntityRecord,
) -> Result<UpdateCamera, String> {
    // Camera updates carry a full or partial `CameraParams` patch.
    // Merge the patch over the existing params so a partial update
    // doesn't zero out other fields.
    let existing = record
        .body
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let merged = match (existing, patch.clone()) {
        (Value::Object(mut base), Value::Object(p)) => {
            for (k, v) in p {
                base.insert(k, v);
            }
            Value::Object(base)
        }
        // If either side isn't an object (shouldn't happen for a
        // well-formed camera entity), prefer the patch verbatim so
        // the AI's intent is honoured.
        (_, p) => p,
    };
    let params: CameraParams = serde_json::from_value(merged)
        .map_err(|e| format!("camera update patch does not match CameraParams: {e}"))?;
    Ok(UpdateCamera {
        entity_id: target,
        params,
    })
}

fn convert_delete(
    op_index: usize,
    target: &EntityId,
    tool: &str,
    graph: &ProjectGraph,
    commands: &mut Vec<Command>,
    skipped: &mut Vec<SkippedOperation>,
) {
    let Some(record) = graph.get(target) else {
        skipped.push(SkippedOperation {
            op_index,
            reason: format!("delete target `{target}` not found in project graph"),
        });
        return;
    };
    let kind = record.kind.as_str();
    if is_furniture_kind(kind) {
        commands.push(Command::ai(
            tool,
            CommandKind::DeleteFurniture(DeleteFurniture {
                entity_id: target.clone(),
            }),
        ));
        return;
    }
    match kind {
        "wall" => commands.push(Command::ai(
            tool,
            CommandKind::DeleteWall(DeleteWall {
                entity_id: target.clone(),
            }),
        )),
        "camera" => commands.push(Command::ai(
            tool,
            CommandKind::DeleteCamera(DeleteCamera {
                entity_id: target.clone(),
            }),
        )),
        "light" => commands.push(Command::ai(
            tool,
            CommandKind::RemoveLight(RemoveLight {
                entity_id: target.clone(),
            }),
        )),
        other => skipped.push(SkippedOperation {
            op_index,
            reason: format!("delete for entity kind `{other}` not yet supported"),
        }),
    }
}

fn parse_xyz(v: Option<&Value>) -> Result<[f64; 3], String> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        // Missing position is acceptable for some payloads (e.g. a
        // furniture insert from a pure asset shelf); default to
        // origin so the model's intent is preserved.
        return Ok([0.0, 0.0, 0.0]);
    };
    if arr.len() < 3 {
        return Err(format!(
            "expected [x, y, z] number triple, got array of length {}",
            arr.len()
        ));
    }
    let parsed = |i: usize| {
        arr[i]
            .as_f64()
            .ok_or_else(|| format!("position component {i} is not a number"))
    };
    Ok([parsed(0)?, parsed(1)?, parsed(2)?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_ai::{DiffStatus, ToolName};
    use aec_core::types::DiffId;
    use serde_json::json;

    fn diff_of(tool: ToolName, ops: Vec<DiffOperation>) -> Diff {
        Diff {
            id: DiffId::new(),
            tool,
            status: DiffStatus::Pending,
            operations: ops,
        }
    }

    fn empty_graph() -> ProjectGraph {
        ProjectGraph::new()
    }

    #[test]
    fn plan_detection_polyline_becomes_n_minus_one_create_walls() {
        let diff = diff_of(
            ToolName::PlanDetection,
            vec![DiffOperation::Insert {
                entity_kind: "wall".into(),
                payload: json!({"points": [[0.0, 0.0], [3000.0, 0.0], [3000.0, 2400.0]]}),
            }],
        );
        let out = diff_to_commands(&diff, &empty_graph(), ApplyDefaults::default());
        assert_eq!(out.commands.len(), 2, "2 segments from 3 points");
        assert!(out.skipped.is_empty());
        for cmd in &out.commands {
            match &cmd.kind {
                CommandKind::CreateWall(cw) => {
                    assert_eq!(cw.height_mm, 2400.0);
                    assert_eq!(cw.thickness_mm, 100.0);
                }
                other => panic!("expected CreateWall, got {other:?}"),
            }
            assert!(
                cmd.actor.is_ai(),
                "ai-derived command must be Ai-attributed"
            );
        }
    }

    #[test]
    fn style_assistant_furniture_inserts_become_place_furniture() {
        let diff = diff_of(
            ToolName::StyleAssistant,
            vec![
                DiffOperation::Insert {
                    entity_kind: "furniture".into(),
                    payload: json!({"asset_id": "ikea.sofa_kivik_3s"}),
                },
                DiffOperation::Insert {
                    entity_kind: "lighting_preset".into(),
                    payload: json!({"preset_id": "warm_evening"}),
                },
            ],
        );
        let out = diff_to_commands(&diff, &empty_graph(), ApplyDefaults::default());
        assert_eq!(out.commands.len(), 2);
        assert!(matches!(
            out.commands[0].kind,
            CommandKind::PlaceFurniture(_)
        ));
        assert!(matches!(out.commands[1].kind, CommandKind::SetLighting(_)));
    }

    #[test]
    fn material_binding_without_target_is_skipped() {
        let diff = diff_of(
            ToolName::StyleAssistant,
            vec![DiffOperation::Insert {
                entity_kind: "material_binding".into(),
                payload: json!({"material_id": "mat_oak"}),
            }],
        );
        let out = diff_to_commands(&diff, &empty_graph(), ApplyDefaults::default());
        assert!(out.commands.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].reason.contains("target_entity"));
    }

    #[test]
    fn unknown_insert_kind_is_skipped_not_panic() {
        let diff = diff_of(
            ToolName::StyleAssistant,
            vec![DiffOperation::Insert {
                entity_kind: "spaceship".into(),
                payload: json!({}),
            }],
        );
        let out = diff_to_commands(&diff, &empty_graph(), ApplyDefaults::default());
        assert!(out.commands.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].reason.contains("spaceship"));
    }

    #[test]
    fn delete_dispatches_by_entity_kind() {
        use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
        let mut g = ProjectGraph::new();
        let furn_id = EntityId::new();
        let wall_id = EntityId::new();
        g.apply(&EntityDelta::Create {
            record: EntityRecord {
                id: furn_id.clone(),
                kind: "furniture".into(),
                body: json!({"asset_ref": "x"}),
                parent: None,
            },
        })
        .unwrap();
        g.apply(&EntityDelta::Create {
            record: EntityRecord {
                id: wall_id.clone(),
                kind: "wall".into(),
                body: json!({}),
                parent: None,
            },
        })
        .unwrap();
        let diff = diff_of(
            ToolName::LayoutSuggestion,
            vec![
                DiffOperation::Delete {
                    target: furn_id.clone(),
                },
                DiffOperation::Delete {
                    target: wall_id.clone(),
                },
            ],
        );
        let out = diff_to_commands(&diff, &g, ApplyDefaults::default());
        assert_eq!(out.commands.len(), 2);
        assert!(matches!(
            out.commands[0].kind,
            CommandKind::DeleteFurniture(_)
        ));
        assert!(matches!(out.commands[1].kind, CommandKind::DeleteWall(_)));
    }

    #[test]
    fn update_furniture_becomes_move_furniture() {
        use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
        let mut g = ProjectGraph::new();
        let furn_id = EntityId::new();
        g.apply(&EntityDelta::Create {
            record: EntityRecord {
                id: furn_id.clone(),
                kind: "furniture".into(),
                body: json!({"asset_ref": "x"}),
                parent: None,
            },
        })
        .unwrap();
        let diff = diff_of(
            ToolName::LayoutSuggestion,
            vec![DiffOperation::Update {
                target: furn_id.clone(),
                patch: json!({"position_mm": [1000.0, 2000.0, 0.0], "rotation_deg": 90.0}),
            }],
        );
        let out = diff_to_commands(&diff, &g, ApplyDefaults::default());
        assert_eq!(out.commands.len(), 1);
        match &out.commands[0].kind {
            CommandKind::MoveFurniture(c) => {
                assert_eq!(c.entity_id, furn_id);
                assert_eq!(c.position_mm, [1000.0, 2000.0, 0.0]);
                assert_eq!(c.rotation_yaw_deg, 90.0);
            }
            other => panic!("expected MoveFurniture, got {other:?}"),
        }
    }

    #[test]
    fn update_dangling_target_is_skipped() {
        let diff = diff_of(
            ToolName::LayoutSuggestion,
            vec![DiffOperation::Update {
                target: EntityId::new(),
                patch: json!({}),
            }],
        );
        let out = diff_to_commands(&diff, &empty_graph(), ApplyDefaults::default());
        assert!(out.commands.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].reason.contains("not found"));
    }
}
