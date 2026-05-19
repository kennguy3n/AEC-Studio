//! DXF tables: LAYER and BLOCK_RECORD.

use serde::{Deserialize, Serialize};

use crate::layers::Layer;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfLayer {
    pub layer: Layer,
    pub handle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfBlockRecord {
    pub name: String,
    pub description: Option<String>,
    pub flags: i32,
}

impl DxfBlockRecord {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            flags: 0,
        }
    }
}
