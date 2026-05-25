//! Conversion between [`crate::primitives::Primitive`] (the modeling
//! representation used by the project graph) and [`DxfEntity`] (the
//! DXF wire-format representation used by the reader/writer).
//!
//! Modeling primitives are 2D — z is always 0 in the DXF output, and
//! the reader's z is dropped when round-tripping. Layer names and
//! bulges round-trip 1:1 in both directions.

use crate::dxf::entities::{
    DxfArc, DxfCircle, DxfEllipse, DxfEntity, DxfLine, DxfPolyline, DxfPolylineVertex, DxfText,
};
use crate::primitives::{Arc, Circle, Ellipse, Line, Polyline, PolylineVertex, Primitive, Text};

/// Convert a modelling [`Primitive`] to a DXF entity.
///
/// Returns `None` for primitives that don't have a direct DXF mapping
/// (currently only `Hatch`, `Spline`, `MText`, which the reader emits
/// but the writer needs domain-specific handling for). Use the new
/// hatch / spline DXF writer paths in `aec_cad::dxf::writer` for those.
pub fn primitive_to_dxf(p: &Primitive) -> Option<DxfEntity> {
    Some(match p {
        Primitive::Line(l) => DxfEntity::Line(DxfLine {
            layer: l.layer.clone(),
            start: [l.start[0], l.start[1], 0.0],
            end: [l.end[0], l.end[1], 0.0],
        }),
        Primitive::Polyline(p) => DxfEntity::Polyline(DxfPolyline {
            layer: p.layer.clone(),
            vertices: p
                .vertices
                .iter()
                .map(|v| DxfPolylineVertex {
                    x: v.at[0],
                    y: v.at[1],
                    bulge: v.bulge,
                })
                .collect(),
            closed: p.closed,
            elevation: p.elevation,
        }),
        Primitive::Arc(a) => DxfEntity::Arc(DxfArc {
            layer: a.layer.clone(),
            center: [a.center[0], a.center[1], 0.0],
            radius: a.radius,
            start_angle: a.start_angle,
            end_angle: a.end_angle,
        }),
        Primitive::Circle(c) => DxfEntity::Circle(DxfCircle {
            layer: c.layer.clone(),
            center: [c.center[0], c.center[1], 0.0],
            radius: c.radius,
        }),
        Primitive::Ellipse(e) => DxfEntity::Ellipse(DxfEllipse {
            layer: e.layer.clone(),
            center: [e.center[0], e.center[1], 0.0],
            major_axis: [e.major[0], e.major[1], 0.0],
            ratio: e.ratio,
            start_param: e.start_param,
            end_param: e.end_param,
        }),
        Primitive::Text(t) => DxfEntity::Text(DxfText {
            layer: t.layer.clone(),
            position: [t.position[0], t.position[1], 0.0],
            height: t.height,
            rotation: t.rotation_deg,
            text: t.content.clone(),
        }),
        // Spline, Hatch, MText: not yet mapped through this helper.
        _ => return None,
    })
}

/// Convert a DXF entity back into a modelling primitive. Drops the
/// `z` component (the project graph is 2D); preserves layer, bulge,
/// closed-ness, and arc/ellipse angles 1:1.
///
/// Returns `None` for DXF entities that don't have a modelling
/// counterpart (currently `Insert`, `Dimension`, `Spline`, `Hatch` —
/// hatch and spline have modelling counterparts but the wire-format
/// representation in `DxfEntity` doesn't preserve enough information
/// to round-trip without the originating block / pattern context;
/// those are imported via dedicated reader paths in
/// `aec_cad::dxf::reader`).
pub fn dxf_to_primitive(d: &DxfEntity) -> Option<Primitive> {
    Some(match d {
        DxfEntity::Line(l) => Primitive::Line(Line {
            layer: l.layer.clone(),
            start: [l.start[0], l.start[1]],
            end: [l.end[0], l.end[1]],
            color_override: None,
            lineweight_override: None,
            linetype_override: None,
        }),
        DxfEntity::Polyline(p) => Primitive::Polyline(Polyline {
            layer: p.layer.clone(),
            vertices: p
                .vertices
                .iter()
                .map(|v| PolylineVertex {
                    at: [v.x, v.y],
                    bulge: v.bulge,
                })
                .collect(),
            closed: p.closed,
            elevation: p.elevation,
            color_override: None,
            lineweight_override: None,
        }),
        DxfEntity::Arc(a) => Primitive::Arc(Arc {
            layer: a.layer.clone(),
            center: [a.center[0], a.center[1]],
            radius: a.radius,
            start_angle: a.start_angle,
            end_angle: a.end_angle,
        }),
        DxfEntity::Circle(c) => Primitive::Circle(Circle {
            layer: c.layer.clone(),
            center: [c.center[0], c.center[1]],
            radius: c.radius,
        }),
        DxfEntity::Ellipse(e) => Primitive::Ellipse(Ellipse {
            layer: e.layer.clone(),
            center: [e.center[0], e.center[1]],
            major: [e.major_axis[0], e.major_axis[1]],
            ratio: e.ratio,
            start_param: e.start_param,
            end_param: e.end_param,
        }),
        DxfEntity::Text(t) => {
            let mut prim = Text::new(
                t.layer.clone(),
                [t.position[0], t.position[1]],
                t.height,
                &t.text,
            );
            prim.rotation_deg = t.rotation;
            Primitive::Text(prim)
        }
        // Insert, Dimension, Spline, Hatch: not yet mapped through this
        // helper. Schedule import for these goes through the dedicated
        // block / dimension / hatch import paths in the reader so we
        // can keep their domain-specific metadata.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_roundtrips() {
        let p = Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 5.0]));
        let dxf = primitive_to_dxf(&p).unwrap();
        let back = dxf_to_primitive(&dxf).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn circle_roundtrips() {
        let p = Primitive::Circle(Circle::new("WALLS", [3.0, 4.0], 5.0));
        let dxf = primitive_to_dxf(&p).unwrap();
        let back = dxf_to_primitive(&dxf).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn polyline_preserves_bulge_and_closed() {
        let pl = Polyline {
            layer: "0".into(),
            vertices: vec![
                PolylineVertex {
                    at: [0.0, 0.0],
                    bulge: 0.0,
                },
                PolylineVertex {
                    at: [10.0, 0.0],
                    bulge: 0.5,
                },
                PolylineVertex {
                    at: [10.0, 10.0],
                    bulge: 0.0,
                },
            ],
            closed: true,
            elevation: 0.0,
            color_override: None,
            lineweight_override: None,
        };
        let p = Primitive::Polyline(pl);
        let dxf = primitive_to_dxf(&p).unwrap();
        let back = dxf_to_primitive(&dxf).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn arc_roundtrips() {
        let mut a = Arc::new("0", [0.0, 0.0], 5.0);
        a.start_angle = 0.0;
        a.end_angle = 90.0;
        let p = Primitive::Arc(a);
        let dxf = primitive_to_dxf(&p).unwrap();
        let back = dxf_to_primitive(&dxf).unwrap();
        assert_eq!(p, back);
    }
}
