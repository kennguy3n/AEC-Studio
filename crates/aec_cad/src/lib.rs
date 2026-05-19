//! `aec_cad` — 2D CAD core.
//!
//! Contains a DXF (R12-compatible ASCII) reader/writer, the layer state
//! manager, and a typed entity/primitives library used by Draft mode.
//!
//! The DXF parser is deliberately scoped to a useful working subset:
//! `LINE`, `POLYLINE`/`LWPOLYLINE`, `ARC`, `CIRCLE`, `TEXT`, `MTEXT`,
//! `DIMENSION`, `INSERT`, plus `LAYER`, `BLOCK_RECORD`, and `DIMSTYLE`
//! tables. This is enough for the Phase 1 round-trip spike and for the
//! drafting MVP. Phase 3 will broaden the parser to streaming + binary DXF
//! and add the long tail of less common groups.

pub mod dxf;
pub mod error;
pub mod layers;
pub mod primitives;

pub use dxf::{DxfDocument, DxfReader, DxfWriter};
pub use error::{CadError, CadResult};
pub use layers::{Layer, LayerColor, LayerLineweight, LayerSystem};
pub use primitives::{Arc, Circle, Polyline, Primitive, Text};
