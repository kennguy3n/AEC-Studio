//! LINE entity codec.
//!
//! Bit-stream layout (R2000-R2018, simplified):
//!
//! ```text
//! common_entity_header                 // shared header
//! z_is_zero: B                         // bit flag: "Z coord is zero" optimisation
//! start.x: BD
//! start.y: BD
//! [start.z: BD]                        // only if !z_is_zero
//! end.x: BD                            // or DD encoding relative to start
//! end.y: BD
//! [end.z: BD]                          // only if !z_is_zero
//! thickness: BT (BD with control bits) // 0 by default
//! extrusion: BE (3BD, 0,0,1 by default)
//! ```
//!
//! This module exposes the structured intermediate type so the top
//! level reader/writer can bridge to [`crate::dxf::DxfLine`].

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dxf::DxfLine;

#[derive(Debug, Clone, PartialEq)]
pub struct LineEntity {
    pub layer: String,
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub thickness: f64,
    pub extrusion: [f64; 3],
}

impl LineEntity {
    pub fn into_dxf(self) -> DxfLine {
        DxfLine {
            layer: self.layer,
            start: self.start,
            end: self.end,
        }
    }

    pub fn from_dxf(dxf: &DxfLine) -> Self {
        Self {
            layer: dxf.layer.clone(),
            start: dxf.start,
            end: dxf.end,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        }
    }

    /// Encode the bit-stream payload (no common entity header).
    /// Used by the top-level writer once it has emitted the shared
    /// header for the entity.
    pub fn encode_payload(&self, w: &mut BitWriter) -> DwgResult<()> {
        let z_is_zero = self.start[2] == 0.0 && self.end[2] == 0.0;
        w.write_b(z_is_zero)?;
        w.write_bd(self.start[0])?;
        w.write_bd(self.start[1])?;
        if !z_is_zero {
            w.write_bd(self.start[2])?;
        }
        w.write_bd(self.end[0])?;
        w.write_bd(self.end[1])?;
        if !z_is_zero {
            w.write_bd(self.end[2])?;
        }
        w.write_bd(self.thickness)?;
        // BE — bit extrusion. Bit=1 means "default (0,0,1)", bit=0 means
        // "explicit 3BD follows".
        let is_default_extrusion = self.extrusion == [0.0, 0.0, 1.0];
        w.write_b(is_default_extrusion)?;
        if !is_default_extrusion {
            w.write_3bd(self.extrusion)?;
        }
        Ok(())
    }

    pub fn decode_payload(r: &mut BitReader<'_>, layer: String) -> DwgResult<Self> {
        let z_is_zero = r.read_b()?;
        let sx = r.read_bd()?;
        let sy = r.read_bd()?;
        let sz = if z_is_zero { 0.0 } else { r.read_bd()? };
        let ex = r.read_bd()?;
        let ey = r.read_bd()?;
        let ez = if z_is_zero { 0.0 } else { r.read_bd()? };
        let thickness = r.read_bd()?;
        let extrusion_default = r.read_b()?;
        let extrusion = if extrusion_default {
            [0.0, 0.0, 1.0]
        } else {
            r.read_3bd()?
        };
        Ok(Self {
            layer,
            start: [sx, sy, sz],
            end: [ex, ey, ez],
            thickness,
            extrusion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_payload_round_trips_z_zero_default_extrusion() {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [100.0, 50.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        line.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = LineEntity::decode_payload(&mut r, "0".into()).unwrap();
        assert_eq!(got, line);
    }

    #[test]
    fn line_payload_round_trips_z_nonzero_custom_extrusion() {
        let line = LineEntity {
            layer: "WALL".into(),
            start: [1.0, 2.0, 3.0],
            end: [4.0, 5.0, 6.0],
            thickness: 0.5,
            extrusion: [1.0, 0.0, 0.0],
        };
        let mut w = BitWriter::new();
        line.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = LineEntity::decode_payload(&mut r, "WALL".into()).unwrap();
        assert_eq!(got, line);
    }

    #[test]
    fn line_dxf_bridge_round_trips() {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 10.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let dxf = line.clone().into_dxf();
        let back = LineEntity::from_dxf(&dxf);
        assert_eq!(back, line);
    }
}
