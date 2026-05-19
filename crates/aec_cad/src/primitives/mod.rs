//! Internal CAD primitive types — what the editor manipulates before
//! converting to/from DXF entities.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub layer: String,
    pub start: [f64; 2],
    pub end: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Polyline {
    pub layer: String,
    pub vertices: Vec<[f64; 2]>,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arc {
    pub layer: String,
    pub center: [f64; 2],
    pub radius: f64,
    /// Degrees, counter-clockwise.
    pub start_angle: f64,
    pub end_angle: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub layer: String,
    pub center: [f64; 2],
    pub radius: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub layer: String,
    pub position: [f64; 2],
    pub height: f64,
    pub rotation_deg: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Primitive {
    Line(Line),
    Polyline(Polyline),
    Arc(Arc),
    Circle(Circle),
    Text(Text),
}
