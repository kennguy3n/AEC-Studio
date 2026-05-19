//! Door / window opening commands. Each carries enough metadata for the
//! geometry crate to compute the wall cut from the host wall.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoorKind {
    SingleSwing,
    DoubleSwing,
    Sliding,
    Pocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Fixed,
    Casement,
    Sliding,
    Awning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceDoor {
    pub entity_id: EntityId,
    pub host_wall_id: EntityId,
    pub position_along_wall_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    pub door_kind: DoorKind,
}

impl PlaceDoor {
    pub fn validate(&self) -> CommandResult<()> {
        if self.width_mm <= 0.0 || self.height_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_door".into(),
                reason: "door dimensions must be positive".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "door".into(),
                body: serde_json::to_value(self).expect("serializable"),
                parent: Some(self.host_wall_id.clone()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceWindow {
    pub entity_id: EntityId,
    pub host_wall_id: EntityId,
    pub position_along_wall_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    pub sill_height_mm: f64,
    pub window_kind: WindowKind,
}

impl PlaceWindow {
    pub fn validate(&self) -> CommandResult<()> {
        if self.width_mm <= 0.0 || self.height_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_window".into(),
                reason: "window dimensions must be positive".into(),
            });
        }
        if self.sill_height_mm < 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.place_window".into(),
                reason: "sill height must be non-negative".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "window".into(),
                body: serde_json::to_value(self).expect("serializable"),
                parent: Some(self.host_wall_id.clone()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveOpening {
    pub entity_id: EntityId,
    pub new_position_along_wall_mm: f64,
}

impl MoveOpening {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        if let Some(obj) = after.as_object_mut() {
            obj.insert(
                "position_along_wall_mm".into(),
                serde_json::json!(self.new_position_along_wall_mm),
            );
        }
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteOpening {
    pub entity_id: EntityId,
}

impl DeleteOpening {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        Ok(EntityDelta::Delete { record: record.clone() })
    }
}
