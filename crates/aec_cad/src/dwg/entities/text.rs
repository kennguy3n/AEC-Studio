//! TEXT entity codec.
//!
//! Bit-stream layout (R2000-R2018) — every optional field is gated on
//! one bit of the leading `data_flags` byte. A flag set to `1` means
//! the corresponding field is **omitted** and the decoder substitutes
//! the documented default. Mirrors the LibreDWG `dwg.spec` definition
//! at lines 100-172 so files written by either codec can be read by
//! the other.
//!
//! ```text
//! data_flags: RC   // 8-bit; bit set → field omitted
//! elevation: RD       if !(data_flags & 0x01)   else 0.0
//! insertion_point: 2RD
//! alignment_pt: 2DD   if !(data_flags & 0x02)   else == insertion_point
//!     (each component defaults to the matching insertion_point component)
//! extrusion: BE
//! thickness: BT
//! oblique_angle: RD   if !(data_flags & 0x04)   else 0.0
//! rotation: RD        if !(data_flags & 0x08)   else 0.0
//! height: RD
//! width_factor: RD    if !(data_flags & 0x10)   else 1.0
//! text_value: TV/T
//! generation: BS      if !(data_flags & 0x20)   else 0
//! horiz_align: BS     if !(data_flags & 0x40)   else 0
//! vert_align: BS      if !(data_flags & 0x80)   else 0
//! ```
//!
//! In the wider DWG record format the text style is a handle in the
//! handle stream rather than an inline string. We serialise it inline
//! here for self-contained round-tripping; the modern bridge layer
//! reattaches the style handle when writing a complete file.

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;
use crate::dxf::DxfText;

const DATA_FLAGS_ELEVATION_OMITTED: u8 = 0x01;
const DATA_FLAGS_ALIGNMENT_OMITTED: u8 = 0x02;
const DATA_FLAGS_OBLIQUE_OMITTED: u8 = 0x04;
const DATA_FLAGS_ROTATION_OMITTED: u8 = 0x08;
const DATA_FLAGS_WIDTH_OMITTED: u8 = 0x10;
const DATA_FLAGS_GENERATION_OMITTED: u8 = 0x20;
const DATA_FLAGS_HORIZ_ALIGN_OMITTED: u8 = 0x40;
const DATA_FLAGS_VERT_ALIGN_OMITTED: u8 = 0x80;

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

    /// Compute the smallest data_flags byte that encodes the omitted
    /// fields for this entity. We omit any field that holds its
    /// documented default value, mirroring what AutoCAD itself does
    /// — this keeps the on-wire payload as compact as the reference
    /// implementation and helps the oracle round-trip test stay
    /// byte-stable.
    fn data_flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.position[2] == 0.0 {
            flags |= DATA_FLAGS_ELEVATION_OMITTED;
        }
        // The alignment point we serialise is the insertion point
        // itself (we don't yet track a separate one), so it always
        // equals the default.
        flags |= DATA_FLAGS_ALIGNMENT_OMITTED;
        if self.oblique_angle == 0.0 {
            flags |= DATA_FLAGS_OBLIQUE_OMITTED;
        }
        if self.rotation == 0.0 {
            flags |= DATA_FLAGS_ROTATION_OMITTED;
        }
        if self.width_factor == 1.0 {
            flags |= DATA_FLAGS_WIDTH_OMITTED;
        }
        // We don't track generation / horiz / vert alignment in the
        // current schema; they always hold their defaults.
        flags |= DATA_FLAGS_GENERATION_OMITTED
            | DATA_FLAGS_HORIZ_ALIGN_OMITTED
            | DATA_FLAGS_VERT_ALIGN_OMITTED;
        flags
    }

    pub fn encode_payload(&self, w: &mut BitWriter, version: Version) -> DwgResult<()> {
        let flags = self.data_flags();
        w.write_bits_u32(8, u32::from(flags))?;
        if flags & DATA_FLAGS_ELEVATION_OMITTED == 0 {
            w.write_rd(self.position[2])?;
        }
        let ins_pt = [self.position[0], self.position[1]];
        w.write_2rd(ins_pt)?;
        if flags & DATA_FLAGS_ALIGNMENT_OMITTED == 0 {
            // No separate alignment point in our schema; emit
            // ins_pt-relative DDs that evaluate to ins_pt.
            w.write_2dd(ins_pt, ins_pt)?;
        }
        // R2000+ extrusion uses the BE shape (1-bit prefix + optional
        // 3 BDs); R14 and earlier use 3 raw BDs. The TEXT entity is
        // R13+ in this codec, and our minimum is R2000, so always
        // emit the BE shape.
        w.write_be_r2000_plus([0.0, 0.0, 1.0])?;
        w.write_bt_r2000_plus(0.0)?;
        if flags & DATA_FLAGS_OBLIQUE_OMITTED == 0 {
            w.write_rd(self.oblique_angle)?;
        }
        if flags & DATA_FLAGS_ROTATION_OMITTED == 0 {
            w.write_rd(self.rotation)?;
        }
        // Height is always present (no bit gates it out).
        w.write_rd(self.height)?;
        if flags & DATA_FLAGS_WIDTH_OMITTED == 0 {
            w.write_rd(self.width_factor)?;
        }
        if version.uses_utf16_strings() {
            w.write_t(&self.text)?;
        } else {
            w.write_tv(&self.text)?;
        }
        if flags & DATA_FLAGS_GENERATION_OMITTED == 0 {
            w.write_bs(0)?;
        }
        if flags & DATA_FLAGS_HORIZ_ALIGN_OMITTED == 0 {
            w.write_bs(0)?;
        }
        if flags & DATA_FLAGS_VERT_ALIGN_OMITTED == 0 {
            w.write_bs(0)?;
        }
        // Style is a handle in real DWG; we serialise the name inline
        // here for self-contained round-tripping. Lives outside the
        // data_flags-gated region.
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
        let data_flags = r.read_bits_u32(8)? as u8;
        let elevation = if data_flags & DATA_FLAGS_ELEVATION_OMITTED == 0 {
            r.read_rd()?
        } else {
            0.0
        };
        let ins_pt = r.read_2rd()?;
        let _alignment_pt = if data_flags & DATA_FLAGS_ALIGNMENT_OMITTED == 0 {
            r.read_2dd(ins_pt)?
        } else {
            ins_pt
        };
        let _extrusion = r.read_be_r2000_plus()?;
        let _thickness = r.read_bt_r2000_plus()?;
        let oblique_angle = if data_flags & DATA_FLAGS_OBLIQUE_OMITTED == 0 {
            r.read_rd()?
        } else {
            0.0
        };
        let rotation = if data_flags & DATA_FLAGS_ROTATION_OMITTED == 0 {
            r.read_rd()?
        } else {
            0.0
        };
        let height = r.read_rd()?;
        let width_factor = if data_flags & DATA_FLAGS_WIDTH_OMITTED == 0 {
            r.read_rd()?
        } else {
            1.0
        };
        let text = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        if data_flags & DATA_FLAGS_GENERATION_OMITTED == 0 {
            let _ = r.read_bs()?;
        }
        if data_flags & DATA_FLAGS_HORIZ_ALIGN_OMITTED == 0 {
            let _ = r.read_bs()?;
        }
        if data_flags & DATA_FLAGS_VERT_ALIGN_OMITTED == 0 {
            let _ = r.read_bs()?;
        }
        let style = if version.uses_utf16_strings() {
            r.read_t()?
        } else {
            r.read_tv()?
        };
        Ok(Self {
            layer,
            position: [ins_pt[0], ins_pt[1], elevation],
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

    #[test]
    fn text_omits_default_fields_via_data_flags() {
        // An all-defaults entity should encode with data_flags=0xff
        // (every optional field omitted). This regression-protects
        // the encoder against re-introducing the previous bug where
        // every optional field was emitted unconditionally,
        // bloating payloads beyond what AutoCAD writes for the
        // same content.
        let t = TextEntity {
            layer: "0".into(),
            position: [0.0, 0.0, 0.0],
            height: 1.0,
            rotation: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
            text: "X".into(),
            style: "Standard".into(),
        };
        assert_eq!(t.data_flags(), 0xff);
    }

    #[test]
    fn text_decode_substitutes_defaults_for_omitted_fields() {
        // Encode with all defaults (data_flags=0xff), then decode.
        // The decoder must reconstruct the same entity using the
        // documented default values for every omitted field — proving
        // that decoding files emitted by AutoCAD (which routinely set
        // data_flags != 0) yields correct results, not corrupted ones.
        let t = TextEntity {
            layer: "0".into(),
            position: [0.0, 0.0, 0.0],
            height: 1.0,
            rotation: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
            text: "X".into(),
            style: "Standard".into(),
        };
        let mut w = BitWriter::new();
        t.encode_payload(&mut w, Version::R2000).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = TextEntity::decode_payload(&mut r, "0".into(), Version::R2000).unwrap();
        assert_eq!(got, t);
    }
}
