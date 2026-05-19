//! Finish-swap engine. The viewport and the AI assistants both produce
//! [`FinishSwap`] requests; the renderer applies the new material id to the
//! addressed surface, and the journal stores the previous id for undo.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishSwap {
    /// Entity the material is attached to (wall, floor, ceiling, asset
    /// instance, etc.).
    pub target_entity: EntityId,
    /// Index into the entity's per-slot material array (0 for primary).
    pub slot: u32,
    pub new_material_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapPreview {
    pub swap: FinishSwap,
    pub previous_material_id: Option<String>,
}

impl FinishSwap {
    /// Build the swap-preview record that the command engine will persist on
    /// `apply`. The `previous` is read from the current entity state.
    pub fn preview(self, previous: Option<String>) -> SwapPreview {
        SwapPreview {
            swap: self,
            previous_material_id: previous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_carries_previous_id() {
        let swap = FinishSwap {
            target_entity: EntityId::new(),
            slot: 0,
            new_material_id: "mat:walnut".into(),
        };
        let preview = swap.clone().preview(Some("mat:oak_light".into()));
        assert_eq!(preview.swap.new_material_id, "mat:walnut");
        assert_eq!(
            preview.previous_material_id.as_deref(),
            Some("mat:oak_light")
        );
    }
}
