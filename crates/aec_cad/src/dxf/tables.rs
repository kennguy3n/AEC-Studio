//! DXF tables: LAYER and BLOCK_RECORD.

use serde::{Deserialize, Serialize};

use super::entities::DxfEntity;
use crate::layers::Layer;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfLayer {
    pub layer: Layer,
    pub handle: Option<String>,
}

/// A `BLOCK_RECORD` table entry plus the body of the block (the
/// entities that get instantiated when a matching `INSERT` is drawn).
///
/// `entities` can contain any normal drawing entity — including
/// nested [`DxfEntity::Insert`] references, which is how nested
/// blocks are modelled — and any number of [`DxfEntity::Attdef`]
/// (attribute definitions) that participate in the block's
/// attribute schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfBlockRecord {
    pub name: String,
    pub description: Option<String>,
    pub flags: i32,
    /// Base point of the block (DXF group codes 10/20/30). Defaults
    /// to the origin.
    #[serde(default)]
    pub base_point: [f64; 3],
    /// Entities that make up the body of this block. Empty for
    /// blocks that only contribute metadata.
    #[serde(default)]
    pub entities: Vec<DxfEntity>,
}

impl DxfBlockRecord {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            flags: 0,
            base_point: [0.0, 0.0, 0.0],
            entities: Vec::new(),
        }
    }
}
