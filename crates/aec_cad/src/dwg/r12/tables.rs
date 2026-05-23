//! R12 (AC1009) named-table records.
//!
//! AutoCAD R12 keeps named tables (LAYER, BLOCK, LTYPE, STYLE, VIEW,
//! UCS, VPORT, DIMSTYLE, APPID) — one section per table — laid out
//! either at fixed offsets immediately after the file header
//! (BLOCK / LAYER / STYLE / LTYPE / VIEW) or at offsets recorded
//! inside the header-variables block (UCS / VPORT / APPID /
//! DIMSTYLE / VX), see
//! [`crate::dwg::r12::spec::section_table`].
//!
//! This module owns the in-memory representation of each table
//! record. The wire encoding of the table sections themselves lives
//! under [`crate::dwg::r12::spec`].
//!
//! PR-F2 emits all ten tables empty (only the BEGIN/END sentinel
//! pair). A follow-up PR will add the per-record wire-encoders that
//! LibreDWG expects (see `dwg.spec :: DWG_OBJECT_TABLE_HDR`).

/// Fixed name field length used by every R12 table record.
pub const R12_NAME_LEN: usize = 32;

/// Layer table record (51 bytes per AC1009 spec).
///
/// Layout: 32-byte name, RC color, RS ltype_index, RC flag.
#[derive(Debug, Clone, PartialEq)]
pub struct R12LayerRecord {
    pub name: String,
    /// AutoCAD color index (signed: -1=invisible, 0=ByBlock, 1..=255).
    pub color: i16,
    /// Linetype table index (1-based).
    pub ltype_index: u16,
    /// Layer flags: 1=frozen, 2=locked, 4=frozen-on-new-vp, 64=in-use.
    pub flag: u8,
}

/// Block table record (61 bytes per AC1009 spec).
///
/// Layout: 32-byte name, RC flag, 2RD insertion_base, RD elevation,
/// RS num_entities, RL entities_section_offset.
#[derive(Debug, Clone, PartialEq)]
pub struct R12BlockRecord {
    pub name: String,
    pub flag: u8,
    pub insertion_base: [f64; 2],
    pub elevation: f64,
    pub num_entities: u16,
    pub entities_offset: u32,
}

/// Linetype table record (43 bytes minimum + dash array).
#[derive(Debug, Clone, PartialEq)]
pub struct R12LinetypeRecord {
    pub name: String,
    pub flag: u8,
    /// Total pattern length (sum of |dash| values).
    pub pattern_length: f64,
    /// Dash array (positive = dash, negative = gap, zero = dot).
    pub dashes: Vec<f64>,
}

/// Style (text-style) table record (50 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12StyleRecord {
    pub name: String,
    pub flag: u8,
    pub fixed_height: f64,
    pub width_factor: f64,
    pub oblique_angle: f64,
    pub generation: u8,
    pub last_height: f64,
}

/// View table record (88 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12ViewRecord {
    pub name: String,
    pub flag: u8,
    pub size: [f64; 2],
    pub center: [f64; 2],
    pub direction: [f64; 3],
    pub target: [f64; 3],
}

/// UCS table record (54 bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct R12UcsRecord {
    pub name: String,
    pub flag: u8,
    pub origin: [f64; 3],
    pub x_axis: [f64; 3],
    pub y_axis: [f64; 3],
}

/// Viewport table record (72 bytes minimum).
#[derive(Debug, Clone, PartialEq)]
pub struct R12VportRecord {
    pub name: String,
    pub flag: u8,
    pub lower_left: [f64; 2],
    pub upper_right: [f64; 2],
    pub center: [f64; 2],
    pub view_size: f64,
    pub aspect_ratio: f64,
}

/// Dimension-style record (variable; we encode the subset our writer
/// needs end-to-end). Layout: 32-byte name + RC flag + RS num_doubles
/// + (num_doubles × RD).
#[derive(Debug, Clone, PartialEq)]
pub struct R12DimstyleRecord {
    pub name: String,
    pub flag: u8,
    /// Numeric DIM-style values in declaration order. The R12 dim
    /// style has 49 doubles; we don't tie the codec to a fixed count
    /// because real files have R12-vs-R13 quirks here.
    pub values: Vec<f64>,
}

/// AppID table record (35 bytes per AC1009 spec).
#[derive(Debug, Clone, PartialEq)]
pub struct R12AppIdRecord {
    pub name: String,
    pub flag: u8,
}

/// Logical table contents, version-agnostic.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct R12Tables {
    pub layers: Vec<R12LayerRecord>,
    pub blocks: Vec<R12BlockRecord>,
    pub linetypes: Vec<R12LinetypeRecord>,
    pub styles: Vec<R12StyleRecord>,
    pub views: Vec<R12ViewRecord>,
    pub ucs: Vec<R12UcsRecord>,
    pub vports: Vec<R12VportRecord>,
    pub dimstyles: Vec<R12DimstyleRecord>,
    pub appids: Vec<R12AppIdRecord>,
}
