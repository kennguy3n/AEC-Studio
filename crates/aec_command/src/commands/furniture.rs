//! Furniture placement commands.
//!
//! Drops a `furniture` entity into the project graph that references a
//! catalogue entry from the [`aec_assets`] asset library. The actual
//! mesh / thumbnail blobs live in the asset library; this entity only
//! carries the catalogue link plus per-instance placement metadata
//! (position, yaw, optional override scale). This is the same shape
//! the renderer's `journey_a` integration test (`crates/aec_command/
//! tests/journey_a.rs`) previously assembled by hand — now it's a
//! first-class `CommandKind` with `validate` + `to_delta` so the AI
//! planner and renderer can emit it via the standard
//! `command_apply` path.
//!
//! The `furniture` entity kind is a sibling to `wall` / `room` /
//! `floor` / `camera` etc.; like them, it's stored in the
//! `entities` table verbatim and round-trips through
//! [`crate::commands::ProjectGraph::load`] / `persist_delta` without
//! a schema change (the graph treats `kind` as opaque text).

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

/// Place a furniture instance referencing a catalogue asset from the
/// `aec_assets` library.
///
/// `asset_ref` is the `AssetMetadata::asset_id` (e.g. `"asset_sofa_01"`).
/// The renderer looks up the mesh / LOD chain / thumbnail via the
/// asset library at scene-assembly time; this command only persists
/// the catalogue link + placement metadata. Decoupling the mesh blob
/// from the project graph keeps the encrypted `project.sqlite` small
/// and lets the asset library evolve independently (e.g. swap LOD
/// chains, regenerate thumbnails) without invalidating the project's
/// entity rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceFurniture {
    /// Stable id for this furniture instance. Distinct from
    /// `asset_ref` because the same asset can be placed many times
    /// (a chair appearing four times around a table is four
    /// `PlaceFurniture` instances all sharing the same `asset_ref`).
    pub entity_id: EntityId,
    /// Catalogue id from `aec_assets::AssetMetadata::asset_id`.
    pub asset_ref: String,
    /// World-space position in millimetres. Matches the unit
    /// convention used by every other `design.*` command.
    pub position_mm: [f64; 3],
    /// Yaw rotation in degrees around the +Z (up) axis. Roll / pitch
    /// are intentionally omitted: furniture is gravity-aligned and
    /// only spins around its vertical axis. If a future use case
    /// requires tilted furniture (e.g. wall-mounted sconces), add a
    /// separate `PlaceWallFixture` command rather than widening this
    /// type.
    #[serde(default)]
    pub rotation_yaw_deg: f64,
    /// Optional per-instance scale override. `None` uses the
    /// catalogue mesh as authored. `Some(s)` multiplies the bounding
    /// box uniformly — useful for resizing demo furniture without
    /// regenerating the asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_override: Option<f64>,
    /// Optional human-readable name. Defaults to the catalogue
    /// `name` at render time if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional parent spatial node (e.g. a room or storey
    /// `EntityId`). When present, the renderer can use this for
    /// scene-graph filtering (`show only Bedroom furniture`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,
}

impl PlaceFurniture {
    pub fn validate(&self) -> CommandResult<()> {
        if self.asset_ref.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_furniture".into(),
                reason: "asset_ref must not be empty".into(),
            });
        }
        if !self.position_mm.iter().all(|c| c.is_finite()) {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_furniture".into(),
                reason: "position_mm components must be finite".into(),
            });
        }
        if !self.rotation_yaw_deg.is_finite() {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_furniture".into(),
                reason: "rotation_yaw_deg must be finite".into(),
            });
        }
        if let Some(s) = self.scale_override {
            if !s.is_finite() || s <= 0.0 {
                return Err(CommandError::InvalidArguments {
                    tool: "design.place_furniture".into(),
                    reason: "scale_override must be a positive finite number".into(),
                });
            }
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "furniture".into(),
                body: serde_json::to_value(self).expect("serializable"),
                parent: self.parent.clone(),
            },
        }
    }
}

/// Move (or re-orient / re-scale) an existing furniture instance.
/// Modelled as an `Update` delta so the journal records the
/// before/after body — undo restores the previous placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveFurniture {
    pub entity_id: EntityId,
    pub position_mm: [f64; 3],
    #[serde(default)]
    pub rotation_yaw_deg: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_override: Option<f64>,
}

impl MoveFurniture {
    pub fn validate(&self) -> CommandResult<()> {
        if !self.position_mm.iter().all(|c| c.is_finite()) {
            return Err(CommandError::InvalidArguments {
                tool: "design.move_furniture".into(),
                reason: "position_mm components must be finite".into(),
            });
        }
        if !self.rotation_yaw_deg.is_finite() {
            return Err(CommandError::InvalidArguments {
                tool: "design.move_furniture".into(),
                reason: "rotation_yaw_deg must be finite".into(),
            });
        }
        if let Some(s) = self.scale_override {
            if !s.is_finite() || s <= 0.0 {
                return Err(CommandError::InvalidArguments {
                    tool: "design.move_furniture".into(),
                    reason: "scale_override must be a positive finite number".into(),
                });
            }
        }
        Ok(())
    }

    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        if record.kind != "furniture" {
            return Err(CommandError::InvalidArguments {
                tool: "design.move_furniture".into(),
                reason: format!(
                    "target entity {} is kind={}; expected `furniture`",
                    self.entity_id, record.kind
                ),
            });
        }
        let mut after = record.body.clone();
        if let Some(obj) = after.as_object_mut() {
            obj.insert("position_mm".into(), serde_json::json!(self.position_mm));
            obj.insert(
                "rotation_yaw_deg".into(),
                serde_json::json!(self.rotation_yaw_deg),
            );
            match self.scale_override {
                Some(s) => {
                    obj.insert("scale_override".into(), serde_json::json!(s));
                }
                None => {
                    obj.remove("scale_override");
                }
            }
        }
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}

/// Remove a furniture instance from the project graph. The catalogue
/// entry in `aec_assets` is unaffected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteFurniture {
    pub entity_id: EntityId,
}

impl DeleteFurniture {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        if record.kind != "furniture" {
            return Err(CommandError::InvalidArguments {
                tool: "design.delete_furniture".into(),
                reason: format!(
                    "target entity {} is kind={}; expected `furniture`",
                    self.entity_id, record.kind
                ),
            });
        }
        Ok(EntityDelta::Delete {
            record: record.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(entity_id: EntityId, asset_ref: &str) -> PlaceFurniture {
        PlaceFurniture {
            entity_id,
            asset_ref: asset_ref.into(),
            position_mm: [1000.0, 2000.0, 0.0],
            rotation_yaw_deg: 90.0,
            scale_override: None,
            name: Some("Chair (kitchen)".into()),
            parent: None,
        }
    }

    #[test]
    fn place_furniture_validates_then_emits_create_delta_with_furniture_kind() {
        let id = EntityId::new();
        let cmd = placement(id.clone(), "asset_chair_oak_01");
        cmd.validate().unwrap();
        let delta = cmd.to_delta();
        match delta {
            EntityDelta::Create { record } => {
                assert_eq!(record.kind, "furniture");
                assert_eq!(record.id, id);
                let body = record.body.as_object().unwrap();
                assert_eq!(
                    body.get("asset_ref").and_then(|v| v.as_str()),
                    Some("asset_chair_oak_01"),
                );
                assert_eq!(
                    body.get("position_mm")
                        .and_then(|v| v.as_array())
                        .map(Vec::len),
                    Some(3),
                );
                assert_eq!(
                    body.get("rotation_yaw_deg")
                        .and_then(serde_json::Value::as_f64),
                    Some(90.0),
                );
                // `name` round-trips when set.
                assert_eq!(
                    body.get("name").and_then(|v| v.as_str()),
                    Some("Chair (kitchen)"),
                );
            }
            other => panic!("expected Create delta, got {other:?}"),
        }
    }

    #[test]
    fn place_furniture_rejects_empty_asset_ref() {
        let cmd = placement(EntityId::new(), "   ");
        let err = cmd.validate().unwrap_err();
        assert!(matches!(err, CommandError::InvalidArguments { .. }));
    }

    #[test]
    fn place_furniture_rejects_non_finite_position() {
        let mut cmd = placement(EntityId::new(), "asset_x");
        cmd.position_mm = [f64::NAN, 0.0, 0.0];
        assert!(matches!(
            cmd.validate(),
            Err(CommandError::InvalidArguments { .. })
        ));
    }

    #[test]
    fn place_furniture_rejects_non_positive_scale_override() {
        let mut cmd = placement(EntityId::new(), "asset_x");
        cmd.scale_override = Some(-1.0);
        assert!(matches!(
            cmd.validate(),
            Err(CommandError::InvalidArguments { .. })
        ));
        cmd.scale_override = Some(0.0);
        assert!(matches!(
            cmd.validate(),
            Err(CommandError::InvalidArguments { .. })
        ));
    }

    #[test]
    fn move_furniture_updates_body_and_records_before_state() {
        let mut g = ProjectGraph::new();
        let id = EntityId::new();
        let create = placement(id.clone(), "asset_y").to_delta();
        g.apply(&create).unwrap();

        let mv = MoveFurniture {
            entity_id: id.clone(),
            position_mm: [5000.0, 0.0, 0.0],
            rotation_yaw_deg: 0.0,
            scale_override: Some(1.5),
        };
        mv.validate().unwrap();
        let delta = mv.to_delta(&g).unwrap();
        match delta {
            EntityDelta::Update {
                id: updated_id,
                before,
                after,
            } => {
                assert_eq!(updated_id, id);
                assert_eq!(
                    before
                        .get("position_mm")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .first()
                        .unwrap()
                        .as_f64(),
                    Some(1000.0),
                );
                assert_eq!(
                    after
                        .get("position_mm")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .first()
                        .unwrap()
                        .as_f64(),
                    Some(5000.0),
                );
                assert_eq!(
                    after
                        .get("scale_override")
                        .and_then(serde_json::Value::as_f64),
                    Some(1.5),
                );
            }
            other => panic!("expected Update delta, got {other:?}"),
        }
    }

    #[test]
    fn move_furniture_rejects_target_of_wrong_kind() {
        // A `wall` entity is not a furniture target.
        let mut g = ProjectGraph::new();
        let wall_id = EntityId::new();
        let wall_record = EntityRecord {
            id: wall_id.clone(),
            kind: "wall".into(),
            body: serde_json::json!({}),
            parent: None,
        };
        g.apply(&EntityDelta::Create {
            record: wall_record,
        })
        .unwrap();
        let mv = MoveFurniture {
            entity_id: wall_id,
            position_mm: [0.0, 0.0, 0.0],
            rotation_yaw_deg: 0.0,
            scale_override: None,
        };
        let err = mv.to_delta(&g).unwrap_err();
        assert!(matches!(err, CommandError::InvalidArguments { .. }));
    }

    #[test]
    fn delete_furniture_round_trips_through_graph() {
        let mut g = ProjectGraph::new();
        let id = EntityId::new();
        g.apply(&placement(id.clone(), "asset_z").to_delta())
            .unwrap();
        let del = DeleteFurniture {
            entity_id: id.clone(),
        };
        let delta = del.to_delta(&g).unwrap();
        assert!(matches!(delta, EntityDelta::Delete { .. }));
        g.apply(&delta).unwrap();
        assert!(g.get(&id).is_none());
    }
}
