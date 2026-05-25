//! DXF (ASCII) reader and writer.

pub mod entities;
pub mod reader;
pub mod tables;
pub mod writer;

pub use entities::{
    DxfArc, DxfAttdef, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfEllipse,
    DxfEntity, DxfHatch, DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline, DxfPolylineVertex,
    DxfSpline, DxfText, DxfTextStyle,
};
pub use reader::DxfReader;
pub use tables::{DxfBlockRecord, DxfLayer};
pub use writer::DxfWriter;

use serde::{Deserialize, Serialize};

use crate::layers::LayerSystem;

/// In-memory DXF document.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfDocument {
    pub layers: LayerSystem,
    pub block_records: Vec<DxfBlockRecord>,
    pub dim_styles: Vec<DxfDimStyle>,
    #[serde(default = "default_text_styles")]
    pub text_styles: Vec<DxfTextStyle>,
    pub entities: Vec<DxfEntity>,
}

fn default_text_styles() -> Vec<DxfTextStyle> {
    vec![DxfTextStyle::standard()]
}

impl DxfDocument {
    pub fn new() -> Self {
        Self {
            layers: LayerSystem::new(),
            block_records: Vec::new(),
            dim_styles: vec![DxfDimStyle::standard()],
            text_styles: vec![DxfTextStyle::standard()],
            entities: Vec::new(),
        }
    }

    pub fn push(&mut self, e: DxfEntity) {
        self.entities.push(e);
    }
}
