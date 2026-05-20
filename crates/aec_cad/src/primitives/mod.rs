//! Internal CAD primitive types — what the editor manipulates before
//! converting to/from DXF entities.
//!
//! Each primitive lives in its own module and implements four traits:
//! [`Drawable`], [`Selectable`], [`Snappable`], and [`Transformable`].

pub mod arc;
pub mod circle;
pub mod ellipse;
pub mod hatch;
pub mod line;
pub mod polyline;
pub mod spline;
pub mod text;
pub mod traits;

pub use arc::Arc;
pub use circle::Circle;
pub use ellipse::Ellipse;
pub use hatch::{Hatch, HatchBoundary, HatchPattern};
pub use line::Line;
pub use polyline::{Polyline, PolylineVertex};
pub use spline::Spline;
pub use text::{HAlign, MText, Text, VAlign};
pub use traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

use serde::{Deserialize, Serialize};

/// Tagged union of every editable primitive in a drawing.
///
/// `Primitive` itself implements the four primitive traits — it dispatches
/// to the inner variant. This keeps the editing and selection code generic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Primitive {
    Line(Line),
    Polyline(Polyline),
    Arc(Arc),
    Circle(Circle),
    Ellipse(Ellipse),
    Spline(Spline),
    Hatch(Hatch),
    Text(Text),
    MText(MText),
}

impl Primitive {
    pub fn layer(&self) -> &str {
        match self {
            Primitive::Line(p) => p.layer(),
            Primitive::Polyline(p) => p.layer(),
            Primitive::Arc(p) => p.layer(),
            Primitive::Circle(p) => p.layer(),
            Primitive::Ellipse(p) => p.layer(),
            Primitive::Spline(p) => p.layer(),
            Primitive::Hatch(p) => p.layer(),
            Primitive::Text(p) => p.layer(),
            Primitive::MText(p) => p.layer(),
        }
    }

    pub fn bbox(&self) -> Bbox {
        match self {
            Primitive::Line(p) => p.bbox(),
            Primitive::Polyline(p) => p.bbox(),
            Primitive::Arc(p) => p.bbox(),
            Primitive::Circle(p) => p.bbox(),
            Primitive::Ellipse(p) => p.bbox(),
            Primitive::Spline(p) => p.bbox(),
            Primitive::Hatch(p) => p.bbox(),
            Primitive::Text(p) => p.bbox(),
            Primitive::MText(p) => p.bbox(),
        }
    }

    pub fn distance2(&self, point: [f64; 2]) -> f64 {
        match self {
            Primitive::Line(p) => p.distance2(point),
            Primitive::Polyline(p) => p.distance2(point),
            Primitive::Arc(p) => p.distance2(point),
            Primitive::Circle(p) => p.distance2(point),
            Primitive::Ellipse(p) => p.distance2(point),
            Primitive::Spline(p) => p.distance2(point),
            Primitive::Hatch(p) => p.distance2(point),
            Primitive::Text(p) => p.distance2(point),
            Primitive::MText(p) => p.distance2(point),
        }
    }

    pub fn inside(&self, window: &Bbox) -> bool {
        match self {
            Primitive::Line(p) => p.inside(window),
            Primitive::Polyline(p) => p.inside(window),
            Primitive::Arc(p) => p.inside(window),
            Primitive::Circle(p) => p.inside(window),
            Primitive::Ellipse(p) => p.inside(window),
            Primitive::Spline(p) => p.inside(window),
            Primitive::Hatch(p) => p.inside(window),
            Primitive::Text(p) => p.inside(window),
            Primitive::MText(p) => p.inside(window),
        }
    }

    pub fn snap_points(&self) -> Vec<SnapPoint> {
        match self {
            Primitive::Line(p) => p.snap_points(),
            Primitive::Polyline(p) => p.snap_points(),
            Primitive::Arc(p) => p.snap_points(),
            Primitive::Circle(p) => p.snap_points(),
            Primitive::Ellipse(p) => p.snap_points(),
            Primitive::Spline(p) => p.snap_points(),
            Primitive::Hatch(p) => p.snap_points(),
            Primitive::Text(p) => p.snap_points(),
            Primitive::MText(p) => p.snap_points(),
        }
    }

    pub fn transformed(&self, t: &Affine2) -> Self {
        match self {
            Primitive::Line(p) => Primitive::Line(p.transformed(t)),
            Primitive::Polyline(p) => Primitive::Polyline(p.transformed(t)),
            Primitive::Arc(p) => Primitive::Arc(p.transformed(t)),
            Primitive::Circle(p) => Primitive::Circle(p.transformed(t)),
            Primitive::Ellipse(p) => Primitive::Ellipse(p.transformed(t)),
            Primitive::Spline(p) => Primitive::Spline(p.transformed(t)),
            Primitive::Hatch(p) => Primitive::Hatch(p.transformed(t)),
            Primitive::Text(p) => Primitive::Text(p.transformed(t)),
            Primitive::MText(p) => Primitive::MText(p.transformed(t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_dispatch_line() {
        let p = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0]));
        assert_eq!(p.layer(), "0");
        let b = p.bbox();
        assert_eq!(b.min, [0.0, 0.0]);
        assert_eq!(b.max, [10.0, 0.0]);
        assert!((p.distance2([5.0, 1.0]).sqrt() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn primitive_dispatch_circle() {
        let p = Primitive::Circle(Circle::new("0", [0.0, 0.0], 5.0));
        let snaps = p.snap_points();
        assert_eq!(snaps.len(), 5);
    }

    #[test]
    fn primitive_roundtrip_serde() {
        let p = Primitive::Arc(Arc::new("Walls", [0.0, 0.0], 1.0));
        let s = serde_json::to_string(&p).unwrap();
        let back: Primitive = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
