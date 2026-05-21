//! TEXT entity codec.
//!
//! Bit-stream layout (R2000-R2018):
//!
//! ```text
//! data_flags: RC                       // 8-bit; bitfield controlling presence of optional fields
//! elevation: RD if !DATA_FLAGS_NO_Z
//! insertion_point: 2RD
//! [alignment_point: 2RD if !DATA_FLAGS_NO_ALIGN]
//! extrusion: BE
//! thickness: BT
//! [oblique_angle: RD if !DATA_FLAGS_NO_OBLIQUE]
//! [rotation: RD if !DATA_FLAGS_NO_ROT]
//! height: RD
//! [width_factor: RD if !DATA_FLAGS_NO_WIDTH]
//! text_value: TV/T
//! [generation: BS]
//! [horiz_align: BS]
//! [vert_align: BS]
//! style_handle: H
//! ```
//!
//! We expose the structured form; the version-specific bit layout is
//! handled inside `encode_payload` / `decode_payload`.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;
use crate::dxf::DxfText;

#[derive(Debug, Clone, PartialEq)]
pub struct TextEntity {
    pub layer: String,
    pub position: [f64; 3],
    pub height: f64,
    pub rotation: f64,
    pub width_factor: f64,
    pub oblique_angle: f64,
    pub text: String,
    pub style: String,
}

impl TextEntity {
    pub fn into_dxf(self) -> DxfText {
        DxfText {
            layer: self.layer,
            position: self.position,
            height: self.height,
            rotation: self.rotation.to_degrees(),
            text: self.text,
        }
    }

    pub fn from_dxf(dxf: &DxfText) -> Self {
        Self {
            layer: dxf.layer.clone(),
            position: dxf.position,
            height: dxf.height,
            rotation: dxf.rotation.to_radians(),
            width_factor: 1.0,
            oblique_angle: 0.0,
            text: dxf.text.clone(),
            style: "Standard".into(),
        }
    }

    pub fn encode_payload(&self, w: &mut BitWriter, version: Version) -> DwgResult<()> {
        // Data flags — explicit fields are most reliable when round-tripping
        // a hand-constructed entity. We always emit elevation, rotation,
        // oblique, and width_factor so the decoder doesn't need to know
        // the data_flags shorthand.
        const NONE_OPT: u8 = 0x00;
        w.write_bits_u32(8, u32::from(NONE_OPT))?;
        w.write_rd(self.position[2])?;
        w.write_2rd([self.position[0], self.position[1]])?;
        w.write_2rd([self.position[0], self.position[1]])?; // alignment_point
        w.write_b(true)?; // default extrusion
        w.write_bd(0.0)?; // thickness
        w.write_rd(self.oblique_angle)?;
        w.write_rd(self.rotation)?;
        w.write_rd(self.height)?;
        w.write_rd(self.width_factor)?;
        if version.uses_utf16_strings() {
            w.write_t(&self.text)?;
        } else {
            w.write_tv(&self.text)?;
        }
        // generation / h-align / v-align defaults
        w.write_bs(0)?;
        w.write_bs(0)?;
        w.write_bs(0)?;
        // Style name length-prefixed; in real DWG the style is a handle
        // — we serialise the name inline so the codec is self-contained.
        if version.uses_utf16_strings() {
            w.write_t(&self.style)?;
        } else {
            w.write_tv(&self.style)?;
        }
        Ok(())
    }

    pub fn decode_payload(
        r: &mut BitReader<'_>,
        layer: String,
        version: Version,
    ) -> DwgResult<Self> {
        let _data_flags = r.read_bits_u32(8)?;
        let elevation = r.read_rd()?;
        let xy = r.read_2rd()?;
        let _align = r.read_2rd()?;
        let _ext_default = r.read_b()?;
        let _thickness = r.read_bd()?;
        let oblique_angle = r.read_rd()?;
        let rotation = r.read_rd()?;
        let height = r.read_rd()?;
        let width_factor = r.read_rd()?;
        let text = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        let _gen = r.read_bs()?;
        let _hor = r.read_bs()?;
        let _ver = r.read_bs()?;
        let style = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        Ok(Self {
            layer,
            position: [xy[0], xy[1], elevation],
            height,
            rotation,
            width_factor,
            oblique_angle,
            text,
            style,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_ascii_r2000() {
        let t = TextEntity {
            layer: "0".into(),
            position: [1.0, 2.0, 3.0],
            height: 2.5,
            rotation: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
            text: "Hello".into(),
            style: "Standard".into(),
        };
        let mut w = BitWriter::new();
        t.encode_payload(&mut w, Version::R2000).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = TextEntity::decode_payload(&mut r, "0".into(), Version::R2000).unwrap();
        assert_eq!(got, t);
    }

    #[test]
    fn text_round_trips_utf16_r2010() {
        let t = TextEntity {
            layer: "PLAN".into(),
            position: [0.0, 0.0, 0.0],
            height: 5.0,
            rotation: std::f64::consts::PI / 4.0,
            width_factor: 0.8,
            oblique_angle: 0.0,
            text: "Cliënt 漢字 🚀".into(),
            style: "Romans".into(),
        };
        let mut w = BitWriter::new();
        t.encode_payload(&mut w, Version::R2010).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = TextEntity::decode_payload(&mut r, "PLAN".into(), Version::R2010).unwrap();
        assert_eq!(got, t);
    }
}
