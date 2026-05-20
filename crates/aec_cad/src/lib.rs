//! `aec_cad` — 2D CAD core.
//!
//! Contains a DXF (R12-compatible ASCII) reader/writer, the layer state
//! manager + linetype/lineweight tables, drawing primitives (line, arc,
//! circle, polyline, ellipse, spline, hatch, text), editing tools
//! (move/copy/rotate/scale/mirror/offset/trim/extend/fillet/chamfer/
//! stretch), precision aids (grid, ortho, polar, object snaps, tracking,
//! parametric constraints + solver), block definitions and instances,
//! associative dimensions and dim styles, sheet layouts, a keyboard
//! command-line parser, and an out-of-process DWG converter adapter.
//!
//! The DXF parser is deliberately scoped to a useful working subset:
//! `LINE`, `POLYLINE`/`LWPOLYLINE`, `ARC`, `CIRCLE`, `ELLIPSE`, `SPLINE`,
//! `TEXT`, `MTEXT`, `DIMENSION`, `HATCH`, `INSERT`, plus `LAYER`,
//! `LTYPE`, `BLOCK_RECORD`, `STYLE`, and `DIMSTYLE` tables. This is
//! enough for the drafting MVP and for lossless roundtrip of the
//! drawings we generate.

pub mod blocks;
pub mod command_line;
pub mod dims;
pub mod dwg_adapter;
pub mod dxf;
pub mod editing;
pub mod error;
pub mod layers;
pub mod precision;
pub mod primitives;
pub mod sheets;

pub use dxf::{DxfDocument, DxfReader, DxfWriter};
pub use error::{CadError, CadResult};
pub use layers::{Layer, LayerColor, LayerLineweight, LayerSystem, Linetype, Lineweight};
pub use primitives::{
    Affine2, Arc, Bbox, Circle, Ellipse, Hatch, HatchBoundary, HatchPattern, Line, MText, Polyline,
    PolylineVertex, Primitive, SnapKind, SnapPoint, Spline, Text,
};
