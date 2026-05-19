//! In-viewport material preview state. Tracks which material is bound to
//! each surface slot of each entity and produces a draw command list the
//! renderer consumes each frame.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MaterialPreviewSlot {
    pub entity: EntityId,
    pub slot: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialPreview {
    bindings: HashMap<MaterialPreviewSlot, String>,
}

impl MaterialPreview {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, slot: MaterialPreviewSlot, material_id: impl Into<String>) {
        self.bindings.insert(slot, material_id.into());
    }

    pub fn unbind(&mut self, slot: &MaterialPreviewSlot) -> bool {
        self.bindings.remove(slot).is_some()
    }

    pub fn get(&self, slot: &MaterialPreviewSlot) -> Option<&str> {
        self.bindings.get(slot).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&MaterialPreviewSlot, &str)> {
        self.bindings.iter().map(|(k, v)| (k, v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_unbind_roundtrip() {
        let mut p = MaterialPreview::new();
        let slot = MaterialPreviewSlot {
            entity: EntityId::new(),
            slot: "floor".into(),
        };
        p.bind(slot.clone(), "mat:oak");
        assert_eq!(p.get(&slot), Some("mat:oak"));
        assert!(p.unbind(&slot));
        assert!(p.get(&slot).is_none());
    }
}
