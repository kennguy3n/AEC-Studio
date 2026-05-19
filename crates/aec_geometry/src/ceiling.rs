//! Ceiling primitive. Same as Floor but rendered downward at `height_mm`.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::error::GeometryResult;
use crate::floor::Floor;
use crate::mesh::Mesh;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ceiling {
    pub id: EntityId,
    pub boundary_mm: Vec<[f64; 2]>,
    pub height_mm: f64,
    pub thickness_mm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<String>,
}

impl Ceiling {
    pub fn tessellate(&self) -> GeometryResult<Mesh> {
        // Re-use the floor tessellator with elevation = height_mm.
        let floor = Floor {
            id: self.id.clone(),
            boundary_mm: self.boundary_mm.clone(),
            thickness_mm: self.thickness_mm,
            material_id: self.material_id.clone(),
            elevation_mm: self.height_mm,
        };
        floor.tessellate()
    }
}
