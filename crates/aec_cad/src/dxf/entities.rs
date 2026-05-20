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
}

impl DxfDimStyle {
    pub fn standard() -> Self {
        Self {
            name: "STANDARD".into(),
            text_height: 2.5,
            arrow_size: 2.5,
            units_scale: 1.0,
        }
    }
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
        }
    }
}
