//! DXF (ASCII) reader and writer.

pub mod convert;
pub mod entities;
pub mod reader;
pub mod tables;
pub mod writer;

pub use convert::{dxf_to_primitive, primitive_to_dxf};
pub use entities::{
    DxfArc, DxfCircle, DxfDimStyle, DxfDimension, DxfDimensionKind, DxfEllipse, DxfEntity,
    DxfHatch, DxfHatchLoop, DxfInsert, DxfLine, DxfPolyline, DxfPolylineVertex, DxfSpline, DxfText,
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
    pub entities: Vec<DxfEntity>,
}

impl DxfDocument {
    pub fn new() -> Self {
        Self {
            layers: LayerSystem::new(),
            block_records: Vec::new(),
            dim_styles: vec![DxfDimStyle::standard()],
            entities: Vec::new(),
        }
    }

    pub fn push(&mut self, e: DxfEntity) {
        self.entities.push(e);
    }
}
