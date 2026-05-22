//! ARC entity codec.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dxf::DxfArc;

#[derive(Debug, Clone, PartialEq)]
pub struct ArcEntity {
    pub layer: String,
    pub center: [f64; 3],
    pub radius: f64,
    pub thickness: f64,
    pub extrusion: [f64; 3],
    /// Start angle in radians (DWG stores radians; the DXF group code
    /// 50 stores degrees — conversion happens at the DXF bridge).
    pub start_angle: f64,
    pub end_angle: f64,
}

impl ArcEntity {
    pub fn into_dxf(self) -> DxfArc {
        DxfArc {
            layer: self.layer,
            center: self.center,
            radius: self.radius,
            start_angle: self.start_angle.to_degrees(),
            end_angle: self.end_angle.to_degrees(),
        }
    }

    pub fn from_dxf(dxf: &DxfArc) -> Self {
        Self {
            layer: dxf.layer.clone(),
            center: dxf.center,
            radius: dxf.radius,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            start_angle: dxf.start_angle.to_radians(),
            end_angle: dxf.end_angle.to_radians(),
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter) -> DwgResult<()> {
        w.write_3bd(self.center)?;
        w.write_bd(self.radius)?;
        w.write_bd(self.thickness)?;
        let default = self.extrusion == [0.0, 0.0, 1.0];
        w.write_b(default)?;
        if !default {
            w.write_3bd(self.extrusion)?;
        }
        w.write_bd(self.start_angle)?;
        w.write_bd(self.end_angle)?;
        Ok(())
    }

    pub fn decode_payload(r: &mut BitReader<'_>, layer: String) -> DwgResult<Self> {
        let center = r.read_3bd()?;
        let radius = r.read_bd()?;
        let thickness = r.read_bd()?;
        let default = r.read_b()?;
        let extrusion = if default {
            [0.0, 0.0, 1.0]
        } else {
            r.read_3bd()?
        };
        let start_angle = r.read_bd()?;
        let end_angle = r.read_bd()?;
        Ok(Self {
            layer,
            center,
            radius,
            thickness,
            extrusion,
            start_angle,
            end_angle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_payload_round_trips() {
        let arc = ArcEntity {
            layer: "0".into(),
            center: [0.0, 0.0, 0.0],
            radius: 10.0,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        };
        let mut w = BitWriter::new();
        arc.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = ArcEntity::decode_payload(&mut r, "0".into()).unwrap();
        assert_eq!(got, arc);
    }

    #[test]
    fn arc_dxf_bridge_converts_angle_units() {
        let arc = ArcEntity {
            layer: "0".into(),
            center: [0.0; 3],
            radius: 1.0,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        };
        let dxf = arc.into_dxf();
        assert!((dxf.start_angle - 0.0).abs() < 1e-9);
        assert!((dxf.end_angle - 180.0).abs() < 1e-9);
    }
}
