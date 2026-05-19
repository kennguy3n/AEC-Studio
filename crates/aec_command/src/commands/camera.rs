//! Camera commands. The viewport crate consumes these to materialize saved
//! cameras; the bridge surfaces them as `DesignApi.saveCamera`.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraParams {
    pub position_mm: [f64; 3],
    pub target_mm: [f64; 3],
    pub focal_length_mm: f64,
    pub exposure_ev: f64,
    pub white_balance_k: u32,
    pub depth_of_field_f: Option<f64>,
    pub aspect_ratio: f64,
}

impl CameraParams {
    pub fn validate(&self) -> CommandResult<()> {
        if self.focal_length_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.save_camera".into(),
                reason: "focal length must be positive".into(),
            });
        }
        if self.aspect_ratio <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.save_camera".into(),
                reason: "aspect ratio must be positive".into(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveCamera {
    pub entity_id: EntityId,
    pub name: String,
    pub params: CameraParams,
}

impl SaveCamera {
    pub fn validate(&self) -> CommandResult<()> {
        if self.name.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "design.save_camera".into(),
                reason: "name must not be empty".into(),
            });
        }
        self.params.validate()
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "camera".into(),
                body: serde_json::to_value(self).expect("serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCamera {
    pub entity_id: EntityId,
    pub params: CameraParams,
}

impl UpdateCamera {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        if let Some(obj) = after.as_object_mut() {
            obj.insert("params".into(), serde_json::to_value(&self.params)?);
        }
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteCamera {
    pub entity_id: EntityId,
}

impl DeleteCamera {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        Ok(EntityDelta::Delete {
            record: record.clone(),
        })
    }
}
