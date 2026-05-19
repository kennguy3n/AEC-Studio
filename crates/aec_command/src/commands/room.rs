//! Room commands. A room is a logical grouping referencing a floor and a
//! set of bounding walls.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateRoom {
    pub entity_id: EntityId,
    pub name: String,
    pub wall_ids: Vec<EntityId>,
    pub floor_id: Option<EntityId>,
    pub ceiling_id: Option<EntityId>,
}

impl CreateRoom {
    pub fn validate(&self) -> CommandResult<()> {
        if self.name.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_room".into(),
                reason: "room name must not be empty".into(),
            });
        }
        if self.wall_ids.len() < 3 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_room".into(),
                reason: "a room needs at least 3 walls".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "room".into(),
                body: serde_json::to_value(self).expect("CreateRoom is always serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModifyRoom {
    pub entity_id: EntityId,
    pub name: Option<String>,
    pub add_wall_ids: Vec<EntityId>,
    pub remove_wall_ids: Vec<EntityId>,
}

impl ModifyRoom {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        let obj = after
            .as_object_mut()
            .ok_or_else(|| CommandError::InvalidArguments {
                tool: "design.modify_room".into(),
                reason: "stored room body is not an object".into(),
            })?;
        if let Some(name) = &self.name {
            obj.insert("name".into(), serde_json::json!(name));
        }
        let mut wall_ids: Vec<EntityId> = obj
            .get("wall_ids")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        wall_ids.retain(|w| !self.remove_wall_ids.contains(w));
        for w in &self.add_wall_ids {
            if !wall_ids.contains(w) {
                wall_ids.push(w.clone());
            }
        }
        obj.insert("wall_ids".into(), serde_json::to_value(&wall_ids)?);
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}
