//! LWPOLYLINE entity codec.
//!
//! LWPOLYLINE was added in R14 — it's a lightweight polyline that stores
//! per-vertex (x, y, bulge) tuples directly inside the entity, no
//! VERTEX seqends needed.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dxf::{DxfPolyline, DxfPolylineVertex};

#[derive(Debug, Clone, PartialEq)]
pub struct LwPolylineEntity {
    pub layer: String,
    pub closed: bool,
    pub elevation: f64,
    pub thickness: f64,
    pub extrusion: [f64; 3],
    pub const_width: f64,
    pub vertices: Vec<DxfPolylineVertex>,
}

impl LwPolylineEntity {
    pub fn into_dxf(self) -> DxfPolyline {
        DxfPolyline {
            layer: self.layer,
            vertices: self.vertices,
            closed: self.closed,
            elevation: self.elevation,
        }
    }

    pub fn from_dxf(dxf: &DxfPolyline) -> Self {
        Self {
            layer: dxf.layer.clone(),
            closed: dxf.closed,
            elevation: dxf.elevation,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            const_width: 0.0,
            vertices: dxf.vertices.clone(),
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter) -> DwgResult<()> {
        // Flags: bit0=closed, bit1=has-elevation, bit2=has-thickness,
        // bit3=has-extrusion, bit4=has-const-width, bit5=has-bulges.
        let has_bulges = self.vertices.iter().any(|v| v.bulge != 0.0);
        let mut flags = 0u8;
        if self.closed {
            flags |= 0x01;
        }
        if self.elevation != 0.0 {
            flags |= 0x02;
        }
        if self.thickness != 0.0 {
            flags |= 0x04;
        }
        if self.extrusion != [0.0, 0.0, 1.0] {
            flags |= 0x08;
        }
        if self.const_width != 0.0 {
            flags |= 0x10;
        }
        if has_bulges {
            flags |= 0x20;
        }
        w.write_bits_u32(8, u32::from(flags))?;
        if self.elevation != 0.0 {
            w.write_bd(self.elevation)?;
        }
        if self.thickness != 0.0 {
            w.write_bd(self.thickness)?;
        }
        if self.extrusion != [0.0, 0.0, 1.0] {
            w.write_3bd(self.extrusion)?;
        }
        if self.const_width != 0.0 {
            w.write_bd(self.const_width)?;
        }
        w.write_bl(self.vertices.len() as i64)?;
        for v in &self.vertices {
            w.write_rd(v.x)?;
            w.write_rd(v.y)?;
        }
        if has_bulges {
            for v in &self.vertices {
                w.write_bd(v.bulge)?;
            }
        }
        Ok(())
    }

    pub fn decode_payload(r: &mut BitReader<'_>, layer: String) -> DwgResult<Self> {
        let flags = r.read_bits_u32(8)? as u8;
        let closed = flags & 0x01 != 0;
        let elevation = if flags & 0x02 != 0 { r.read_bd()? } else { 0.0 };
        let thickness = if flags & 0x04 != 0 { r.read_bd()? } else { 0.0 };
        let extrusion = if flags & 0x08 != 0 {
            r.read_3bd()?
        } else {
            [0.0, 0.0, 1.0]
        };
        let const_width = if flags & 0x10 != 0 { r.read_bd()? } else { 0.0 };
        let n = r.read_bl()? as usize;
        let mut vertices: Vec<DxfPolylineVertex> = Vec::with_capacity(n);
        for _ in 0..n {
            let x = r.read_rd()?;
            let y = r.read_rd()?;
            vertices.push(DxfPolylineVertex { x, y, bulge: 0.0 });
        }
        if flags & 0x20 != 0 {
            for v in vertices.iter_mut() {
                v.bulge = r.read_bd()?;
            }
        }
        Ok(Self {
            layer,
            closed,
            elevation,
            thickness,
            extrusion,
            const_width,
            vertices,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lwpolyline_round_trips_simple_open() {
        let pl = LwPolylineEntity {
            layer: "0".into(),
            closed: false,
            elevation: 0.0,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            const_width: 0.0,
            vertices: vec![
                DxfPolylineVertex::new(0.0, 0.0),
                DxfPolylineVertex::new(10.0, 0.0),
                DxfPolylineVertex::new(10.0, 10.0),
            ],
        };
        let mut w = BitWriter::new();
        pl.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = LwPolylineEntity::decode_payload(&mut r, "0".into()).unwrap();
        assert_eq!(got, pl);
    }

    #[test]
    fn lwpolyline_round_trips_closed_with_bulges() {
        let pl = LwPolylineEntity {
            layer: "WALL".into(),
            closed: true,
            elevation: 1.0,
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
            const_width: 0.5,
            vertices: vec![
                DxfPolylineVertex {
                    x: 0.0,
                    y: 0.0,
                    bulge: 0.5,
                },
                DxfPolylineVertex {
                    x: 5.0,
                    y: 5.0,
                    bulge: 0.0,
                },
                DxfPolylineVertex {
                    x: 10.0,
                    y: 0.0,
                    bulge: -0.5,
                },
            ],
        };
        let mut w = BitWriter::new();
        pl.encode_payload(&mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = LwPolylineEntity::decode_payload(&mut r, "WALL".into()).unwrap();
        assert_eq!(got, pl);
    }
}
