//! Wall-related commands.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateWall {
    pub entity_id: EntityId,
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
    pub height_mm: f64,
    pub thickness_mm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
}

impl CreateWall {
    pub fn validate(&self) -> CommandResult<()> {
        if self.height_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_wall".into(),
                reason: "height must be > 0".into(),
            });
        }
        if self.thickness_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_wall".into(),
                reason: "thickness must be > 0".into(),
            });
        }
        if self.start_mm == self.end_mm {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_wall".into(),
                reason: "start and end must differ".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "wall".into(),
                body: serde_json::to_value(self).expect("CreateWall is always serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveWall {
    pub entity_id: EntityId,
    pub new_start_mm: [f64; 2],
    pub new_end_mm: [f64; 2],
}

impl MoveWall {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        if let Some(obj) = after.as_object_mut() {
            obj.insert("start_mm".into(), serde_json::json!(self.new_start_mm));
            obj.insert("end_mm".into(), serde_json::json!(self.new_end_mm));
        }
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteWall {
    pub entity_id: EntityId,
}

impl DeleteWall {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        Ok(EntityDelta::Delete {
            record: record.clone(),
        })
    }
}
