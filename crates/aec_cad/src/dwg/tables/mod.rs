//! Symbol-table sections inside the OBJECTS data — LAYER, BLOCK_RECORD,
//! STYLE, DIMSTYLE, LTYPE, VPORT.
//!
//! Each table is a `_CONTROL_OBJ` (the table itself) plus a sequence
//! of `_TABLE_RECORD` entries. Records share the modern entity header
//! ("common entity data") with the rest of the OBJECTS section.

pub mod block;
pub mod dim_style;
pub mod layer;
pub mod linetype;
pub mod style;

pub use block::BlockRecord;
pub use dim_style::DimStyleRecord;
pub use layer::LayerRecord;
pub use linetype::LinetypeRecord;
pub use style::StyleRecord;
