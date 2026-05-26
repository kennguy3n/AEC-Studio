//! Typed DXF entity structs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfLine {
    pub layer: String,
    pub start: [f64; 3],
    pub end: [f64; 3],
}

/// One vertex of an LWPOLYLINE, with optional bulge for an arc segment
/// to the next vertex (DXF group code 42; tangent of one-quarter the
/// included angle).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DxfPolylineVertex {
    pub x: f64,
    pub y: f64,
    pub bulge: f64,
}

impl DxfPolylineVertex {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y, bulge: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfPolyline {
    pub layer: String,
    pub vertices: Vec<DxfPolylineVertex>,
    pub closed: bool,
    pub elevation: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfArc {
    pub layer: String,
    pub center: [f64; 3],
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfCircle {
    pub layer: String,
    pub center: [f64; 3],
    pub radius: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfEllipse {
    pub layer: String,
    pub center: [f64; 3],
    pub major_axis: [f64; 3],
    pub ratio: f64,
    pub start_param: f64,
    pub end_param: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfSpline {
    pub layer: String,
    pub degree: i32,
    pub knots: Vec<f64>,
    pub control_points: Vec<[f64; 3]>,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfHatchLoop {
    pub vertices: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfHatch {
    pub layer: String,
    pub pattern_name: String,
    pub solid: bool,
    pub scale: f64,
    pub angle: f64,
    pub elevation: f64,
    pub loops: Vec<DxfHatchLoop>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfText {
    pub layer: String,
    pub position: [f64; 3],
    pub height: f64,
    pub rotation: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfInsert {
    pub layer: String,
    pub block_name: String,
    pub position: [f64; 3],
    pub scale: [f64; 3],
    pub rotation: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DxfDimensionKind {
    Aligned,
    Linear,
    Angular,
    Radial,
    Diameter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfDimension {
    pub layer: String,
    pub style: String,
    pub kind: DxfDimensionKind,
    pub def_point: [f64; 3],
    pub text_position: [f64; 3],
    pub def_point_a: [f64; 3],
    pub def_point_b: [f64; 3],
    pub override_text: Option<String>,
    pub measured_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfDimStyle {
    pub name: String,
    pub text_height: f64,
    pub arrow_size: f64,
    pub units_scale: f64,
    /// `DIMDEC` — number of decimal places for the primary
    /// dimension value. AutoCAD defaults to `4`.
    #[serde(default = "default_dim_decimals")]
    pub decimal_places: u8,
    /// `DIMTXSTY` — name of the text style used for dimension text.
    /// Empty string falls back to the default `STANDARD` style.
    #[serde(default)]
    pub text_style: String,
}

fn default_dim_decimals() -> u8 {
    4
}

impl DxfDimStyle {
    pub fn standard() -> Self {
        Self {
            name: "STANDARD".into(),
            text_height: 2.5,
            arrow_size: 2.5,
            units_scale: 1.0,
            decimal_places: 4,
            text_style: "STANDARD".into(),
        }
    }
}

/// A DXF `STYLE` table entry — defines a text style by primary font
/// filename, fixed height (`0.0` means user-set per entity), width
/// factor, oblique angle, and the optional "big font" filename used
/// for CJK character sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfTextStyle {
    pub name: String,
    /// Primary font filename (DXF code 3). Typically `arial.ttf`,
    /// `romans.shx`, etc.
    pub font_filename: String,
    /// Optional bigfont filename for CJK characters (DXF code 4).
    /// Empty when no bigfont is configured.
    #[serde(default)]
    pub bigfont_filename: String,
    /// Fixed text height in drawing units. `0.0` means the style does
    /// not lock the height; per-entity text height takes effect.
    pub fixed_height: f64,
    /// Width factor (horizontal scale of glyphs). `1.0` is the default.
    pub width_factor: f64,
    /// Oblique angle in degrees (`0.0` = upright).
    pub oblique_angle: f64,
}

impl DxfTextStyle {
    pub fn standard() -> Self {
        Self {
            name: "STANDARD".into(),
            font_filename: "txt".into(),
            bigfont_filename: String::new(),
            fixed_height: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
        }
    }
}

/// An `ATTDEF` (attribute definition) entity. Lives inside a block
/// record and describes a slot that an `INSERT` instance can later
/// populate with a value. The four fields below correspond to the
/// DXF group codes 1 (default value), 2 (tag), 3 (prompt) and 70
/// (flags).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfAttdef {
    pub layer: String,
    pub position: [f64; 3],
    pub height: f64,
    pub rotation: f64,
    pub default_value: String,
    pub tag: String,
    pub prompt: String,
    /// Bitfield: 1 = invisible, 2 = constant, 4 = verify, 8 = preset.
    pub flags: i32,
    /// Text style name (DXF code 7); empty falls back to `STANDARD`.
    #[serde(default)]
    pub text_style: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "UPPERCASE")]
pub enum DxfEntity {
    Line(DxfLine),
    Polyline(DxfPolyline),
    Arc(DxfArc),
    Circle(DxfCircle),
    Ellipse(DxfEllipse),
    Spline(DxfSpline),
    Hatch(DxfHatch),
    Text(DxfText),
    Insert(DxfInsert),
    Dimension(DxfDimension),
    Attdef(DxfAttdef),
}

impl DxfEntity {
    pub fn layer(&self) -> &str {
        match self {
            Self::Line(e) => &e.layer,
            Self::Polyline(e) => &e.layer,
            Self::Arc(e) => &e.layer,
            Self::Circle(e) => &e.layer,
            Self::Ellipse(e) => &e.layer,
            Self::Spline(e) => &e.layer,
            Self::Hatch(e) => &e.layer,
            Self::Text(e) => &e.layer,
            Self::Insert(e) => &e.layer,
            Self::Dimension(e) => &e.layer,
            Self::Attdef(e) => &e.layer,
        }
    }
}
