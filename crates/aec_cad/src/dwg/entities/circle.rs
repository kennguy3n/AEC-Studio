//! CIRCLE entity codec.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dxf::DxfCircle;

#[derive(Debug, Clone, PartialEq)]
pub struct CircleEntity {
    pub layer: String,
    pub center: [f64; 3],
    pub radius: f64,
    pub thickness: f64,
    pub extrusion: [f64; 3],
}

impl CircleEntity {
    pub fn into_dxf(self) -> DxfCircle {
        DxfCircle {
            layer: self.layer,
            center: self.center,
            radius: self.radius,
        }
    }

    pub fn from_dxf(dxf: &DxfCircle) -> Self {
        Self {
            layer: dxf.layer.clone(),
            center: dxf.center,
            radius: dxf.radius,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter) -> DwgResult<()> {
        w.write_3bd(self.center)?;
        w.write_bd(self.radius)?;
        w.write_bd(self.thickness)?;
        let is_default = self.extrusion == [0.0, 0.0, 1.0];
        w.write_b(is_default)?;
        if !is_default {
            w.write_3bd(self.extrusion)?;
        }
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
        Ok(Self {
            layer,
            center,
            radius,
            thickness,
            extrusion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_payload_round_trips() {
        let c = CircleEntity {
            layer: "0".into(),
            center: [1.0, 2.0, 3.0],
            radius: 5.0,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        c.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = CircleEntity::decode_payload(&mut r, "0".into()).unwrap();
        assert_eq!(got, c);
    }
}
