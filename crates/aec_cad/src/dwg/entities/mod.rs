//! DWG entity codecs.
//!
//! Each module here implements one entity type's bit-level layout —
//! encoder + decoder + a structured intermediate type that bridges to
//! [`crate::dxf::DxfEntity`].

pub mod arc;
pub mod circle;
pub mod common;
pub mod ellipse;
pub mod header_codec;
pub mod insert;
pub mod line;
pub mod lwpolyline;
pub mod mtext;
pub mod text;

pub use common::CommonEntityHeader;
pub use header_codec::{CommonHeaderData, EntityMode, LinetypeFlag};

/// Numeric object type tag stored inside every entity's bit stream.
/// These IDs are AutoCAD's `OBJECT_TYPE` enum (see OpenDesign spec
/// § "Object type values").  We list the ones we encode/decode; other
/// types are tolerated opaquely on read and dropped on write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ObjectType {
    Text = 0x01,
    Attrib = 0x02,
    AttDef = 0x03,
    Block = 0x04,
    EndBlk = 0x05,
    SeqEnd = 0x06,
    Insert = 0x07,
    MInsert = 0x08,
    Vertex2d = 0x0a,
    Vertex3d = 0x0b,
    VertexMesh = 0x0c,
    VertexPFace = 0x0d,
    VertexPFaceFace = 0x0e,
    Polyline2d = 0x0f,
    Polyline3d = 0x10,
    Arc = 0x11,
    Circle = 0x12,
    Line = 0x13,
    DimensionOrdinate = 0x14,
    DimensionLinear = 0x15,
    DimensionAligned = 0x16,
    DimensionAng3Pt = 0x17,
    DimensionAng2Ln = 0x18,
    DimensionRadius = 0x19,
    DimensionDiameter = 0x1a,
    Point = 0x1b,
    Face3D = 0x1c,
    PolylinePFace = 0x1d,
    PolylineMesh = 0x1e,
    Solid = 0x1f,
    Trace = 0x20,
    Shape = 0x21,
    Viewport = 0x22,
    Ellipse = 0x23,
    Spline = 0x24,
    Region = 0x25,
    Body = 0x27,
    Ray = 0x28,
    XLine = 0x29,
    Dictionary = 0x2a,
    MText = 0x2c,
    Leader = 0x2d,
    Tolerance = 0x2e,
    MLine = 0x2f,
    BlockControl = 0x30,
    BlockHeader = 0x31,
    LayerControl = 0x32,
    Layer = 0x33,
    StyleControl = 0x34,
    Style = 0x35,
    LinetypeControl = 0x38,
    Linetype = 0x39,
    ViewControl = 0x3c,
    View = 0x3d,
    UcsControl = 0x3e,
    Ucs = 0x3f,
    VPortControl = 0x40,
    VPort = 0x41,
    AppIdControl = 0x42,
    AppId = 0x43,
    DimStyleControl = 0x44,
    DimStyle = 0x45,
    VPortEntityHeader = 0x46,
    VPortEntityControl = 0x47,
    LwPolyline = 0x4e,
    Hatch = 0x4f,
    XRecord = 0x50,
}

impl ObjectType {
    pub fn from_u16(v: u16) -> Option<Self> {
        // Manual mapping avoids transmuting an arbitrary u16 into the
        // enum (which would be UB for non-listed values).
        use ObjectType::{
            AppId, AppIdControl, Arc, AttDef, Attrib, Block, BlockControl, BlockHeader, Body,
            Circle, Dictionary, DimStyle, DimStyleControl, DimensionAligned, DimensionAng2Ln,
            DimensionAng3Pt, DimensionDiameter, DimensionLinear, DimensionOrdinate,
            DimensionRadius, Ellipse, EndBlk, Face3D, Hatch, Insert, Layer, LayerControl, Leader,
            Line, Linetype, LinetypeControl, LwPolyline, MInsert, MLine, MText, Point, Polyline2d,
            Polyline3d, PolylineMesh, PolylinePFace, Ray, Region, SeqEnd, Shape, Solid, Spline,
            Style, StyleControl, Text, Tolerance, Trace, Ucs, UcsControl, VPort, VPortControl,
            VPortEntityControl, VPortEntityHeader, Vertex2d, Vertex3d, VertexMesh, VertexPFace,
            VertexPFaceFace, View, ViewControl, Viewport, XLine, XRecord,
        };
        let mapped = match v {
            0x01 => Text,
            0x02 => Attrib,
            0x03 => AttDef,
            0x04 => Block,
            0x05 => EndBlk,
            0x06 => SeqEnd,
            0x07 => Insert,
            0x08 => MInsert,
            0x0a => Vertex2d,
            0x0b => Vertex3d,
            0x0c => VertexMesh,
            0x0d => VertexPFace,
            0x0e => VertexPFaceFace,
            0x0f => Polyline2d,
            0x10 => Polyline3d,
            0x11 => Arc,
            0x12 => Circle,
            0x13 => Line,
            0x14 => DimensionOrdinate,
            0x15 => DimensionLinear,
            0x16 => DimensionAligned,
            0x17 => DimensionAng3Pt,
            0x18 => DimensionAng2Ln,
            0x19 => DimensionRadius,
            0x1a => DimensionDiameter,
            0x1b => Point,
            0x1c => Face3D,
            0x1d => PolylinePFace,
            0x1e => PolylineMesh,
            0x1f => Solid,
            0x20 => Trace,
            0x21 => Shape,
            0x22 => Viewport,
            0x23 => Ellipse,
            0x24 => Spline,
            0x25 => Region,
            0x27 => Body,
            0x28 => Ray,
            0x29 => XLine,
            0x2a => Dictionary,
            0x2c => MText,
            0x2d => Leader,
            0x2e => Tolerance,
            0x2f => MLine,
            0x30 => BlockControl,
            0x31 => BlockHeader,
            0x32 => LayerControl,
            0x33 => Layer,
            0x34 => StyleControl,
            0x35 => Style,
            0x38 => LinetypeControl,
            0x39 => Linetype,
            0x3c => ViewControl,
            0x3d => View,
            0x3e => UcsControl,
            0x3f => Ucs,
            0x40 => VPortControl,
            0x41 => VPort,
            0x42 => AppIdControl,
            0x43 => AppId,
            0x44 => DimStyleControl,
            0x45 => DimStyle,
            0x46 => VPortEntityHeader,
            0x47 => VPortEntityControl,
            0x4e => LwPolyline,
            0x4f => Hatch,
            0x50 => XRecord,
            _ => return None,
        };
        Some(mapped)
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_type_round_trips_known_codes() {
        for v in [
            ObjectType::Line,
            ObjectType::Circle,
            ObjectType::Arc,
            ObjectType::Text,
            ObjectType::MText,
            ObjectType::Insert,
            ObjectType::LwPolyline,
            ObjectType::Layer,
            ObjectType::BlockHeader,
            ObjectType::Spline,
            ObjectType::Ellipse,
        ] {
            let raw = v.as_u16();
            assert_eq!(
                ObjectType::from_u16(raw),
                Some(v),
                "round-trip failed for {v:?}"
            );
        }
    }

    #[test]
    fn object_type_unknown_returns_none() {
        // 0xffff is reserved.
        assert!(ObjectType::from_u16(0xffff).is_none());
    }
}
