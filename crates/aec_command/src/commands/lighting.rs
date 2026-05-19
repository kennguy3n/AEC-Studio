//! Lighting commands.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "params")]
pub enum LightKind {
    SunSky {
        azimuth_deg: f64,
        elevation_deg: f64,
        intensity: f64,
        color_temperature_k: u32,
    },
    Area {
        position_mm: [f64; 3],
        normal: [f64; 3],
        width_mm: f64,
        height_mm: f64,
        intensity: f64,
        color_temperature_k: u32,
    },
    Point {
        position_mm: [f64; 3],
        intensity: f64,
        color_temperature_k: u32,
    },
    Ies {
        position_mm: [f64; 3],
        direction: [f64; 3],
        intensity: f64,
        ies_profile_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetLighting {
    pub preset_id: String,
}

impl SetLighting {
    pub fn validate(&self) -> CommandResult<()> {
        if self.preset_id.trim().is_empty() {
            return Err(CommandError::InvalidArguments {
                tool: "design.set_lighting".into(),
                reason: "preset_id must not be empty".into(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddLight {
    pub entity_id: EntityId,
    pub light: LightKind,
}

impl AddLight {
    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "light".into(),
                body: serde_json::to_value(&self.light).expect("serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveLight {
    pub entity_id: EntityId,
}

impl RemoveLight {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        Ok(EntityDelta::Delete {
            record: record.clone(),
        })
    }
}
