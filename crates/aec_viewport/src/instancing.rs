//! GPU instanced draw batching for repeated furniture.

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Instance {
    /// Row-major 4x4 world transform.
    pub transform: [[f32; 4]; 4],
    pub tint_rgba: [f32; 4],
    pub material_index: u32,
    /// Padding to ensure 16-byte alignment for the next instance.
    pub pad: [u32; 3],
}

impl Instance {
    pub fn identity(material_index: u32) -> Self {
        Self {
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            tint_rgba: [1.0; 4],
            material_index,
            pad: [0; 3],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceBatch {
    pub mesh_id: String,
    pub entities: Vec<EntityId>,
    pub instances: Vec<[[f32; 4]; 4]>,
    pub material_ids: Vec<String>,
}

impl InstanceBatch {
    pub fn new(mesh_id: impl Into<String>) -> Self {
        Self {
            mesh_id: mesh_id.into(),
            entities: Vec::new(),
            instances: Vec::new(),
            material_ids: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        entity: EntityId,
        transform: [[f32; 4]; 4],
        material_id: impl Into<String>,
    ) {
        self.entities.push(entity);
        self.instances.push(transform);
        self.material_ids.push(material_id.into());
    }

    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_pod_layout_matches_expected_size() {
        // 16 floats (transform) + 4 floats (tint) + 4 u32 (mat idx + pad)
        // = 16*4 + 4*4 + 4*4 = 96 bytes.
        assert_eq!(std::mem::size_of::<Instance>(), 96);
    }

    #[test]
    fn batch_accumulates_instances() {
        let mut b = InstanceBatch::new("mesh:sofa");
        b.push(EntityId::new(), [[0.0; 4]; 4], "mat:linen");
        b.push(EntityId::new(), [[0.0; 4]; 4], "mat:linen");
        assert_eq!(b.instance_count(), 2);
    }
}
