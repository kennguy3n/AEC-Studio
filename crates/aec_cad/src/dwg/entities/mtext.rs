//! MTEXT entity codec.
//!
//! MTEXT (multi-line text) is much richer than TEXT — it supports
//! formatting codes (`\P` newlines, `\fArial|b1;…`, etc.) inside a
//! single string and has explicit width/height bounding-box fields.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;

#[derive(Debug, Clone, PartialEq)]
pub struct MTextEntity {
    pub layer: String,
    pub insertion_point: [f64; 3],
    pub extrusion: [f64; 3],
    pub x_axis_direction: [f64; 3],
    pub rect_width: f64,
    pub rect_height: f64,
    pub text_height: f64,
    pub attachment_point: i32,
    pub draw_direction: i32,
    pub text: String,
    pub style: String,
}

impl MTextEntity {
    pub fn encode_payload(&self, w: &mut BitWriter, version: Version) -> DwgResult<()> {
        w.write_3bd(self.insertion_point)?;
        let default_ext = self.extrusion == [0.0, 0.0, 1.0];
        w.write_b(default_ext)?;
        if !default_ext {
            w.write_3bd(self.extrusion)?;
        }
        w.write_3bd(self.x_axis_direction)?;
        w.write_bd(self.rect_width)?;
        w.write_bd(self.rect_height)?;
        w.write_bd(self.text_height)?;
        w.write_bs(self.attachment_point)?;
        w.write_bs(self.draw_direction)?;
        if version.uses_utf16_strings() {
            w.write_t(&self.text)?;
            w.write_t(&self.style)?;
        } else {
            w.write_tv(&self.text)?;
            w.write_tv(&self.style)?;
        }
        Ok(())
    }

    pub fn decode_payload(
        r: &mut BitReader<'_>,
        layer: String,
        version: Version,
    ) -> DwgResult<Self> {
        let insertion_point = r.read_3bd()?;
        let default_ext = r.read_b()?;
        let extrusion = if default_ext {
            [0.0, 0.0, 1.0]
        } else {
            r.read_3bd()?
        };
        let x_axis_direction = r.read_3bd()?;
        let rect_width = r.read_bd()?;
        let rect_height = r.read_bd()?;
        let text_height = r.read_bd()?;
        let attachment_point = r.read_bs()?;
        let draw_direction = r.read_bs()?;
        let text = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        let style = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        Ok(Self {
            layer,
            insertion_point,
            extrusion,
            x_axis_direction,
            rect_width,
            rect_height,
            text_height,
            attachment_point,
            draw_direction,
            text,
            style,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtext_round_trips_r2000() {
        let m = MTextEntity {
            layer: "0".into(),
            insertion_point: [10.0, 20.0, 0.0],
            extrusion: [0.0, 0.0, 1.0],
            x_axis_direction: [1.0, 0.0, 0.0],
            rect_width: 100.0,
            rect_height: 0.0,
            text_height: 2.5,
            attachment_point: 1,
            draw_direction: 1,
            text: "Line 1\\PLine 2".into(),
            style: "Standard".into(),
        };
        let mut w = BitWriter::new();
        m.encode_payload(&mut w, Version::R2000).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = MTextEntity::decode_payload(&mut r, "0".into(), Version::R2000).unwrap();
        assert_eq!(got, m);
    }

    #[test]
    fn mtext_round_trips_r2010_utf16() {
        let m = MTextEntity {
            layer: "ANNO".into(),
            insertion_point: [1.0, 2.0, 3.0],
            extrusion: [0.0, 0.0, 1.0],
            x_axis_direction: [1.0, 0.0, 0.0],
            rect_width: 200.0,
            rect_height: 0.0,
            text_height: 3.0,
            attachment_point: 2,
            draw_direction: 1,
            text: "漢字テスト".into(),
            style: "Standard".into(),
        };
        let mut w = BitWriter::new();
        m.encode_payload(&mut w, Version::R2010).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = MTextEntity::decode_payload(&mut r, "ANNO".into(), Version::R2010).unwrap();
        assert_eq!(got, m);
    }
}
