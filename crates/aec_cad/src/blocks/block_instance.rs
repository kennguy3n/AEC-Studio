//! Block reference (INSERT) — a placement of a block in the drawing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::blocks::block::Block;
use crate::primitives::{Affine2, Primitive};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockInstance {
    pub block_name: String,
    pub layer: String,
    pub insertion: [f64; 2],
    pub scale: [f64; 2],
    pub rotation_deg: f64,
    /// Per-instance attribute value overrides keyed by attribute tag.
    pub attribute_values: BTreeMap<String, String>,
}

impl BlockInstance {
    pub fn new(block_name: impl Into<String>, layer: impl Into<String>) -> Self {
        Self {
            block_name: block_name.into(),
            layer: layer.into(),
            insertion: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation_deg: 0.0,
            attribute_values: BTreeMap::new(),
        }
    }

    pub fn set_attribute(&mut self, tag: impl Into<String>, value: impl Into<String>) {
        self.attribute_values.insert(tag.into(), value.into());
    }

    pub fn attribute(&self, tag: &str, fallback: &Block) -> Option<String> {
        if let Some(v) = self.attribute_values.get(tag) {
            return Some(v.clone());
        }
        fallback
            .attributes
            .iter()
            .find(|a| a.tag == tag)
            .map(|a| a.default_value.clone())
    }

    /// Expand the instance to flat primitives in model space.
    pub fn expand(&self, block: &Block) -> Vec<Primitive> {
        // Translate so block base point is at origin, scale, rotate,
        // then translate to the insertion point. We compose with a
        // single Affine2 by chaining pivot-relative transforms.
        let pre = Affine2 {
            translate: [
                self.insertion[0] - self.scale[0] * 0.0
                    + self.scale[0] * (-Self::base_offset(block)[0]),
                self.insertion[1] + self.scale[1] * (-Self::base_offset(block)[1]),
            ],
            rotation_deg: self.rotation_deg,
            scale: self.scale,
            pivot: self.insertion,
        };
        // Simpler approach: compose with two passes per primitive.
        let _ = pre;
        block
            .entities
            .iter()
            .map(|p| {
                // First translate so block base_point lands at the origin.
                let p1 = p.transformed(&Affine2 {
                    translate: [-block.base_point[0], -block.base_point[1]],
                    ..Affine2::identity()
                });
                // Then scale about origin.
                let p2 = p1.transformed(&Affine2 {
                    scale: self.scale,
                    ..Affine2::identity()
                });
                // Then rotate about origin.
                let p3 = p2.transformed(&Affine2 {
                    rotation_deg: self.rotation_deg,
                    ..Affine2::identity()
                });
                // Finally translate to insertion.
                p3.transformed(&Affine2 {
                    translate: self.insertion,
                    ..Affine2::identity()
                })
            })
            .collect()
    }

    fn base_offset(block: &Block) -> [f64; 2] {
        block.base_point
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Line;

    #[test]
    fn instance_expands_with_translation() {
        let mut block = Block::new("CHAIR");
        block.add_entity(Primitive::Line(Line::new("0", [0.0, 0.0], [1.0, 0.0])));
        let mut inst = BlockInstance::new("CHAIR", "0");
        inst.insertion = [5.0, 5.0];
        let expanded = inst.expand(&block);
        if let Primitive::Line(l) = &expanded[0] {
            assert!((l.start[0] - 5.0).abs() < 1e-9);
            assert!((l.end[0] - 6.0).abs() < 1e-9);
            assert!((l.end[1] - 5.0).abs() < 1e-9);
        }
    }

    #[test]
    fn instance_expands_with_scale_and_rotation() {
        let mut block = Block::new("ARROW");
        block.add_entity(Primitive::Line(Line::new("0", [0.0, 0.0], [1.0, 0.0])));
        let mut inst = BlockInstance::new("ARROW", "0");
        inst.scale = [2.0, 2.0];
        inst.rotation_deg = 90.0;
        let expanded = inst.expand(&block);
        if let Primitive::Line(l) = &expanded[0] {
            assert!((l.end[0]).abs() < 1e-6);
            assert!((l.end[1] - 2.0).abs() < 1e-6);
        }
    }

    #[test]
    fn attribute_lookup_falls_back_to_default() {
        let mut block = Block::new("DOOR");
        block.add_attribute(
            crate::blocks::block_attribute::BlockAttribute::new("MARK", "Door Mark")
                .with_default("D-100"),
        );
        let inst = BlockInstance::new("DOOR", "0");
        assert_eq!(inst.attribute("MARK", &block).unwrap(), "D-100");
    }
}
