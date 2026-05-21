//! INSERT (block reference) entity codec.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;
use crate::dxf::DxfInsert;

#[derive(Debug, Clone, PartialEq)]
pub struct InsertEntity {
    pub layer: String,
    pub block_name: String,
    pub position: [f64; 3],
    pub scale: [f64; 3],
    pub rotation: f64,
    pub extrusion: [f64; 3],
}

impl InsertEntity {
    pub fn into_dxf(self) -> DxfInsert {
        DxfInsert {
            layer: self.layer,
            block_name: self.block_name,
            position: self.position,
            scale: self.scale,
            rotation: self.rotation.to_degrees(),
        }
    }

    pub fn from_dxf(dxf: &DxfInsert) -> Self {
        Self {
            layer: dxf.layer.clone(),
            block_name: dxf.block_name.clone(),
            position: dxf.position,
            scale: dxf.scale,
            rotation: dxf.rotation.to_radians(),
            extrusion: [0.0, 0.0, 1.0],
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter, version: Version) -> DwgResult<()> {
        w.write_3bd(self.position)?;
        // Scale flags: 00=full 3BD, 01=skip Z, 10=skip Y+Z, 11=uniform.
        if self.scale[0] == 1.0 && self.scale[1] == 1.0 && self.scale[2] == 1.0 {
            w.write_bb(0b11)?;
        } else if self.scale[0] == self.scale[1] && self.scale[0] == self.scale[2] {
            w.write_bb(0b10)?;
            w.write_bd(self.scale[0])?;
        } else if self.scale[2] == 1.0 {
            w.write_bb(0b01)?;
            w.write_bd(self.scale[0])?;
            w.write_bd(self.scale[1])?;
        } else {
            w.write_bb(0b00)?;
            w.write_3bd(self.scale)?;
        }
        w.write_bd(self.rotation)?;
        let default_ext = self.extrusion == [0.0, 0.0, 1.0];
        w.write_b(default_ext)?;
        if !default_ext {
            w.write_3bd(self.extrusion)?;
        }
        // Block name — in a real DWG this is a handle reference to a
        // BLOCK_HEADER record. We inline the name so the codec is
        // self-contained for tests; the top-level writer resolves it
        // to a handle when serialising a full document.
        if version.uses_utf16_strings() {
            w.write_t(&self.block_name)?;
        } else {
            w.write_tv(&self.block_name)?;
        }
        Ok(())
    }

    pub fn decode_payload(
        r: &mut BitReader<'_>,
        layer: String,
        version: Version,
    ) -> DwgResult<Self> {
        let position = r.read_3bd()?;
        let scale = match r.read_bb()? {
            0b11 => [1.0, 1.0, 1.0],
            0b10 => {
                let s = r.read_bd()?;
                [s, s, s]
            }
            0b01 => {
                let x = r.read_bd()?;
                let y = r.read_bd()?;
                [x, y, 1.0]
            }
            _ => r.read_3bd()?,
        };
        let rotation = r.read_bd()?;
        let default_ext = r.read_b()?;
        let extrusion = if default_ext {
            [0.0, 0.0, 1.0]
        } else {
            r.read_3bd()?
        };
        let block_name = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        Ok(Self {
            layer,
            block_name,
            position,
            scale,
            rotation,
            extrusion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_round_trips_unit_scale() {
        let ins = InsertEntity {
            layer: "0".into(),
            block_name: "WINDOW".into(),
            position: [100.0, 200.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            rotation: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        ins.encode_payload(&mut w, Version::R2000).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = InsertEntity::decode_payload(&mut r, "0".into(), Version::R2000).unwrap();
        assert_eq!(got, ins);
    }

    #[test]
    fn insert_round_trips_uniform_scale() {
        let ins = InsertEntity {
            layer: "0".into(),
            block_name: "DOOR".into(),
            position: [50.0, 50.0, 0.0],
            scale: [2.0, 2.0, 2.0],
            rotation: std::f64::consts::PI / 2.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        ins.encode_payload(&mut w, Version::R2000).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = InsertEntity::decode_payload(&mut r, "0".into(), Version::R2000).unwrap();
        assert_eq!(got, ins);
    }

    #[test]
    fn insert_round_trips_xyz_scale_utf16() {
        let ins = InsertEntity {
            layer: "FURN".into(),
            block_name: "Tëst-Block".into(),
            position: [0.0, 0.0, 0.0],
            scale: [1.5, 2.5, 0.5],
            rotation: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut w = BitWriter::new();
        ins.encode_payload(&mut w, Version::R2010).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = InsertEntity::decode_payload(&mut r, "FURN".into(), Version::R2010).unwrap();
        assert_eq!(got, ins);
    }
}
