//! BLOCK_HEADER (table) + BLOCK / ENDBLK codec.
//!
//! A block in DWG is a named, parameterised group of entities. Each
//! `BLOCK_HEADER` table record references a list of entity handles
//! that make up the block's geometry, plus a base-point and a flag
//! word identifying anonymous / unnamed blocks (`*Model_Space`,
//! `*Paper_Space`, `*A1`, …).

use crate::dxf::DxfBlockRecord;

#[derive(Debug, Clone, PartialEq)]
pub struct BlockRecord {
    pub name: String,
    pub anonymous: bool,
    pub base_point: [f64; 3],
    pub description: String,
    /// Handles of the entities that belong to this block, in
    /// emission order.  Resolution into actual [`crate::dxf::DxfEntity`]
    /// instances happens after the OBJECTS section finishes parsing.
    pub entity_handles: Vec<u64>,
}

impl BlockRecord {
    pub fn into_dxf(self) -> DxfBlockRecord {
        DxfBlockRecord {
            name: self.name,
            description: if self.description.is_empty() {
                None
            } else {
                Some(self.description)
            },
            flags: i32::from(self.anonymous),
            // The DWG path doesn't yet recover block bodies or base
            // points (entity-graph decoding lands in a later commit);
            // start them at the canonical defaults so the downstream
            // DXF writer emits a syntactically-valid BLOCK … ENDBLK.
            base_point: [0.0, 0.0, 0.0],
            entities: Vec::new(),
        }
    }

    pub fn from_dxf(dxf: &DxfBlockRecord) -> Self {
        Self {
            name: dxf.name.clone(),
            anonymous: dxf.flags & 1 != 0,
            base_point: [0.0, 0.0, 0.0],
            description: dxf.description.clone().unwrap_or_default(),
            entity_handles: Vec::new(),
        }
    }
}
