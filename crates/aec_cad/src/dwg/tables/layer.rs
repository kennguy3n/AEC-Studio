//! LAYER_TABLE_RECORD codec.
//!
//! In the modern (R13+) DWG format, each layer record carries:
//! - common entity data (handle, color, linetype handle, etc.)
//! - T/TV name string
//! - flags (frozen / on / locked / plottable)
//! - linetype handle
//! - line weight (BS)
//!
//! The DXF mirror is [`crate::layers::Layer`]; we round-trip a useful
//! subset (name, color index, lineweight, frozen/locked flags) and
//! preserve everything else through the opaque-handle path.

use crate::layers::{Layer, LayerColor, LayerLineweight};

/// Decoded LAYER table record, ready to be merged into a
/// [`crate::layers::LayerSystem`].
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRecord {
    pub name: String,
    pub frozen: bool,
    pub locked: bool,
    pub plottable: bool,
    pub color: LayerColor,
    pub lineweight: LayerLineweight,
}

impl LayerRecord {
    /// Lift to a `Layer` for insertion into a [`crate::layers::LayerSystem`].
    pub fn into_layer(self) -> Layer {
        Layer {
            name: self.name,
            color: self.color,
            linetype: "CONTINUOUS".into(),
            lineweight: self.lineweight,
            frozen: self.frozen,
            locked: self.locked,
            plottable: self.plottable,
            on: true,
            description: None,
        }
    }

    /// Lower from an existing `Layer`. Used by the writer when
    /// serialising an in-memory `DxfDocument` back to DWG bytes.
    pub fn from_layer(l: &Layer) -> Self {
        Self {
            name: l.name.clone(),
            frozen: l.frozen,
            locked: l.locked,
            plottable: l.plottable,
            color: l.color,
            lineweight: l.lineweight,
        }
    }
}
