//! Floor commands.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateFloor {
    pub entity_id: EntityId,
    pub boundary_mm: Vec<[f64; 2]>,
    pub thickness_mm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
}

impl CreateFloor {
    pub fn validate(&self) -> CommandResult<()> {
        // See `CreateWall::validate` for the rationale behind the
        // `is_finite()` guards (Devin Review `ANALYSIS_0008` on
        // PR #51).
        if self.boundary_mm.len() < 3 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_floor".into(),
                reason: "boundary needs at least 3 points".into(),
            });
        }
        if !self
            .boundary_mm
            .iter()
            .all(|p| p.iter().all(|c| c.is_finite()))
        {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_floor".into(),
                reason: "boundary_mm coordinates must all be finite".into(),
            });
        }
        if !self.thickness_mm.is_finite() || self.thickness_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_floor".into(),
                reason: "thickness must be a finite positive number".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "floor".into(),
                body: serde_json::to_value(self).expect("CreateFloor is serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModifyFloor {
    pub entity_id: EntityId,
    pub new_boundary_mm: Option<Vec<[f64; 2]>>,
    pub new_thickness_mm: Option<f64>,
    pub new_material_id: Option<String>,
}

impl ModifyFloor {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        let obj = after
            .as_object_mut()
            .ok_or_else(|| CommandError::InvalidArguments {
                tool: "design.modify_floor".into(),
                reason: "stored floor body is not an object".into(),
            })?;
        if let Some(b) = &self.new_boundary_mm {
            obj.insert("boundary_mm".into(), serde_json::to_value(b)?);
        }
        if let Some(t) = self.new_thickness_mm {
            obj.insert("thickness_mm".into(), serde_json::json!(t));
        }
        if let Some(m) = &self.new_material_id {
            obj.insert("material_id".into(), serde_json::json!(m));
        }
        Ok(EntityDelta::Update {
            id: self.entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}
