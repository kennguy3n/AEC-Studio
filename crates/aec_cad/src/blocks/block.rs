//! Block definition — a reusable group of primitives anchored to a base
//! point.

use serde::{Deserialize, Serialize};

use crate::blocks::block_attribute::BlockAttribute;
use crate::primitives::Primitive;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub name: String,
    /// Insertion base point in model space.
    pub base_point: [f64; 2],
    pub entities: Vec<Primitive>,
    pub attributes: Vec<BlockAttribute>,
    #[serde(default)]
    pub description: Option<String>,
    /// Anonymous block flag (DXF `*A1234`-style internal names).
    #[serde(default)]
    pub anonymous: bool,
}

impl Block {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_point: [0.0, 0.0],
            entities: Vec::new(),
            attributes: Vec::new(),
            description: None,
            anonymous: false,
        }
    }

    pub fn add_entity(&mut self, prim: Primitive) {
        self.entities.push(prim);
    }

    pub fn add_attribute(&mut self, attr: BlockAttribute) {
        self.attributes.push(attr);
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::Line;

    #[test]
    fn block_entity_count() {
        let mut b = Block::new("WALL");
        b.add_entity(Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])));
        b.add_entity(Primitive::Line(Line::new("0", [10.0, 0.0], [10.0, 3.0])));
        assert_eq!(b.entity_count(), 2);
    }
}
