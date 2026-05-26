//! DXF (ASCII) reader and writer.

pub mod convert;
pub mod entities;
pub mod reader;
pub mod tables;
pub mod writer;

pub use convert::{dxf_to_primitive, primitive_to_dxf};
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfDocument {
    pub layers: LayerSystem,
    pub block_records: Vec<DxfBlockRecord>,
    #[serde(default = "default_dim_styles")]
    pub dim_styles: Vec<DxfDimStyle>,
    #[serde(default = "default_text_styles")]
    pub text_styles: Vec<DxfTextStyle>,
    pub entities: Vec<DxfEntity>,
}

fn default_text_styles() -> Vec<DxfTextStyle> {
    vec![DxfTextStyle::standard()]
}

fn default_dim_styles() -> Vec<DxfDimStyle> {
    vec![DxfDimStyle::standard()]
}

impl Default for DxfDocument {
    /// Returns the same seeded document as [`DxfDocument::new`] —
    /// a default `LayerSystem` (which auto-inserts layer `"0"`),
    /// and the standard STYLE / DIMSTYLE seed records.
    ///
    /// Implemented manually rather than via `#[derive(Default)]` so
    /// every code path that reaches a default document — including
    /// `Default::default()` calls, `..Default::default()` struct
    /// updates, and serde's `#[serde(default)]` field fallback for
    /// callers deserializing partial DXF JSON — sees the same seed
    /// records `new()` emits. A derived `Default` would silently
    /// give `Vec::new()` for both `dim_styles` and `text_styles`,
    /// diverging from `new()` and from the serde field-default
    /// helpers below.
    fn default() -> Self {
        Self::new()
    }
}

impl DxfDocument {
    pub fn new() -> Self {
        Self {
            layers: LayerSystem::new(),
            block_records: Vec::new(),
            dim_styles: default_dim_styles(),
            text_styles: default_text_styles(),
            entities: Vec::new(),
        }
    }

    pub fn push(&mut self, e: DxfEntity) {
        self.entities.push(e);
    }
}
