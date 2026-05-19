//! Material painting commands.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintMaterial {
    pub target_entity_id: EntityId,
    pub material_id: String,
    /// Optional sub-surface (e.g. "wall:interior", "floor:top").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
}

impl PaintMaterial {
    pub fn validate(&self) -> CommandResult<()> {
        if self.material_id.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "design.paint_material".into(),
                reason: "material_id must not be empty".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.target_entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.target_entity_id.to_string()))?;
        let mut after = record.body.clone();
        if let Some(obj) = after.as_object_mut() {
            match &self.surface {
                None => {
                    obj.insert("material_id".into(), serde_json::json!(self.material_id));
                }
                Some(surface) => {
                    let mut surfaces = obj
                        .get("surface_materials")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if let Some(map) = surfaces.as_object_mut() {
                        map.insert(surface.clone(), serde_json::json!(self.material_id));
                    }
                    obj.insert("surface_materials".into(), surfaces);
                }
            }
        }
        Ok(EntityDelta::Update {
            id: self.target_entity_id.clone(),
            before: record.body.clone(),
            after,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapFinish {
    pub target_entity_id: EntityId,
    pub from_material_id: String,
    pub to_material_id: String,
}

impl SwapFinish {
    pub fn to_paint(&self) -> PaintMaterial {
        PaintMaterial {
            target_entity_id: self.target_entity_id.clone(),
            material_id: self.to_material_id.clone(),
            surface: None,
        }
    }
}
