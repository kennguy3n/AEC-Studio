//! Typed DXF entity structs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfLine {
    pub layer: String,
    pub start: [f64; 3],
    pub end: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DxfPolyline {
    pub layer: String,
    pub vertices: Vec<[f64; 2]>,
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
    Text(DxfText),
    Insert(DxfInsert),
}

impl DxfEntity {
    pub fn layer(&self) -> &str {
        match self {
            Self::Line(e) => &e.layer,
            Self::Polyline(e) => &e.layer,
            Self::Arc(e) => &e.layer,
            Self::Circle(e) => &e.layer,
            Self::Text(e) => &e.layer,
            Self::Insert(e) => &e.layer,
        }
    }
}
