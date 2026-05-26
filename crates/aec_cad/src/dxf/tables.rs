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
    /// Layer the BLOCK entity itself sits on (DXF group code 8 on
    /// the BLOCK record inside the BLOCKS section). Per the DXF
    /// spec this is a required field on a BLOCK entity; strict
    /// third-party consumers (AutoCAD, BricsCAD, LibreDWG) reject
    /// or warn on a missing code-8. AutoCAD's convention is that
    /// block definitions live on layer "0" so a contained entity's
    /// `BYLAYER` color resolves through the insert's layer instead
    /// of being baked in at block-definition time.
    #[serde(default = "default_block_layer")]
    pub layer: String,
    /// Entities that make up the body of this block. Empty for
    /// blocks that only contribute metadata.
    #[serde(default)]
    pub entities: Vec<DxfEntity>,
}

fn default_block_layer() -> String {
    "0".to_string()
}

impl DxfBlockRecord {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            flags: 0,
            base_point: [0.0, 0.0, 0.0],
            layer: default_block_layer(),
            entities: Vec::new(),
        }
    }
}
