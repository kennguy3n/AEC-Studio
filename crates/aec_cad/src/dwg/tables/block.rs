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
            // BLOCK entities live on layer "0" by AutoCAD convention
            // so contained `BYLAYER` colors resolve through the
            // INSERT's layer at draw time. DWG BLOCK_HEADER records
            // don't carry this directly — it's a property of the
            // emitted BLOCK entity, not the table record — so we
            // emit the canonical default here. Recovering a
            // non-default value from a DWG file would require the
            // block-body codec to forward the BLOCK entity's code-8
            // through this struct (future work, tracked alongside
            // `entity_handles` resolution).
            layer: "0".to_string(),
            entities: Vec::new(),
        }
    }

    pub fn from_dxf(dxf: &DxfBlockRecord) -> Self {
        Self {
            name: dxf.name.clone(),
            anonymous: dxf.flags & 1 != 0,
            // Mirror the DXF-side `base_point` so that the DWG encoder
            // can emit it once the block-body bit-codec lands. Older
            // revisions of this function hardcoded a zero vector,
            // which would silently lose the block's origin when any
            // call site (none today) flows DxfBlockRecord through
            // BlockRecord on the write side.
            base_point: dxf.base_point,
            description: dxf.description.clone().unwrap_or_default(),
            entity_handles: Vec::new(),
        }
    }
}
