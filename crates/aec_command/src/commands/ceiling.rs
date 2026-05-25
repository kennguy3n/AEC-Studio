//! Ceiling commands.
//!
//! Ceilings are first-class entities, mirroring [`crate::commands::floor`]
//! but at the top of a room. A ceiling has a 2D footprint boundary plus a
//! finish thickness, just like a floor; the elevation at which it sits is
//! the elevation of its room's storey plus the room's `height_mm`.
//!
//! The shipped templates ([`aec_core::templates::TemplateDefinition`]) all
//! list a `height_mm` per room, so templates instantiate exactly one
//! ceiling per room. The room record's `ceiling_id` field (already present
//! in [`crate::commands::room::CreateRoom`]) is then populated with this
//! ceiling's [`EntityId`].

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::commands::{EntityDelta, EntityRecord, ProjectGraph};
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateCeiling {
    pub entity_id: EntityId,
    /// Closed-polygon footprint in mm. At least 3 vertices.
    pub boundary_mm: Vec<[f64; 2]>,
    /// Finish + structure thickness in mm. Must be positive.
    pub thickness_mm: f64,
    /// Default material id (e.g. `"ceiling_white"`). `None` when the
    /// template author did not preset a finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
}

impl CreateCeiling {
    pub fn validate(&self) -> CommandResult<()> {
        if self.boundary_mm.len() < 3 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_ceiling".into(),
                reason: "boundary needs at least 3 points".into(),
            });
        }
        if self.thickness_mm <= 0.0 {
            return Err(CommandError::InvalidArguments {
                tool: "design.create_ceiling".into(),
                reason: "thickness must be > 0".into(),
            });
        }
        Ok(())
    }

    pub fn to_delta(&self) -> EntityDelta {
        EntityDelta::Create {
            record: EntityRecord {
                id: self.entity_id.clone(),
                kind: "ceiling".into(),
                body: serde_json::to_value(self).expect("CreateCeiling is serializable"),
                parent: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModifyCeiling {
    pub entity_id: EntityId,
    pub new_boundary_mm: Option<Vec<[f64; 2]>>,
    pub new_thickness_mm: Option<f64>,
    pub new_material_id: Option<String>,
}

impl ModifyCeiling {
    pub fn to_delta(&self, graph: &ProjectGraph) -> CommandResult<EntityDelta> {
        let record = graph
            .get(&self.entity_id)
            .ok_or_else(|| CommandError::EntityNotFound(self.entity_id.to_string()))?;
        let mut after = record.body.clone();
        let obj = after
            .as_object_mut()
            .ok_or_else(|| CommandError::InvalidArguments {
                tool: "design.modify_ceiling".into(),
                reason: "stored ceiling body is not an object".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_minimum_vertices() {
        let c = CreateCeiling {
            entity_id: EntityId::new(),
            boundary_mm: vec![[0.0, 0.0], [1000.0, 0.0]],
            thickness_mm: 30.0,
            material_id: None,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn validates_positive_thickness() {
        let c = CreateCeiling {
            entity_id: EntityId::new(),
            boundary_mm: vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0]],
            thickness_mm: 0.0,
            material_id: None,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn to_delta_emits_create_with_ceiling_kind() {
        let id = EntityId::new();
        let c = CreateCeiling {
            entity_id: id.clone(),
            boundary_mm: vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]],
            thickness_mm: 30.0,
            material_id: Some("ceiling_white".to_string()),
        };
        let delta = c.to_delta();
        match delta {
            EntityDelta::Create { record } => {
                assert_eq!(record.kind, "ceiling");
                assert_eq!(record.id, id);
                assert_eq!(
                    record.body.get("material_id").and_then(|v| v.as_str()),
                    Some("ceiling_white")
                );
            }
            _ => panic!("expected EntityDelta::Create"),
        }
    }
}
