//! ELLIPSE entity codec.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dxf::DxfEllipse;

#[derive(Debug, Clone, PartialEq)]
pub struct EllipseEntity {
    pub layer: String,
    pub center: [f64; 3],
    pub major_axis: [f64; 3],
    pub extrusion: [f64; 3],
    pub ratio: f64,
    pub start_param: f64,
    pub end_param: f64,
}

impl EllipseEntity {
    pub fn into_dxf(self) -> DxfEllipse {
        DxfEllipse {
            layer: self.layer,
            center: self.center,
            major_axis: self.major_axis,
            ratio: self.ratio,
            start_param: self.start_param,
            end_param: self.end_param,
        }
    }

    pub fn from_dxf(dxf: &DxfEllipse) -> Self {
        Self {
            layer: dxf.layer.clone(),
            center: dxf.center,
            major_axis: dxf.major_axis,
            extrusion: [0.0, 0.0, 1.0],
            ratio: dxf.ratio,
            start_param: dxf.start_param,
            end_param: dxf.end_param,
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter) -> DwgResult<()> {
        w.write_3bd(self.center)?;
        w.write_3bd(self.major_axis)?;
        w.write_3bd(self.extrusion)?;
        w.write_bd(self.ratio)?;
        w.write_bd(self.start_param)?;
        w.write_bd(self.end_param)?;
        Ok(())
    }

    pub fn decode_payload(r: &mut BitReader<'_>, layer: String) -> DwgResult<Self> {
        let center = r.read_3bd()?;
        let major_axis = r.read_3bd()?;
        let extrusion = r.read_3bd()?;
        let ratio = r.read_bd()?;
        let start_param = r.read_bd()?;
        let end_param = r.read_bd()?;
        Ok(Self {
            layer,
            center,
            major_axis,
            extrusion,
            ratio,
            start_param,
            end_param,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipse_round_trips() {
        let e = EllipseEntity {
            layer: "0".into(),
            center: [1.0, 2.0, 3.0],
            major_axis: [10.0, 0.0, 0.0],
            extrusion: [0.0, 0.0, 1.0],
            ratio: 0.5,
            start_param: 0.0,
            end_param: std::f64::consts::TAU,
        };
        let mut w = BitWriter::new();
        e.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = EllipseEntity::decode_payload(&mut r, "0".into()).unwrap();
        assert_eq!(got, e);
    }
}
