//! R13-R2000 classes section.
//!
//! Each non-fixed-class object in a DWG file carries a class code that
//! is dereferenced through this section. The wire format is:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ Start sentinel (16 bytes — CLASSES_BEGIN)            │
//! ├──────────────────────────────────────────────────────┤
//! │ RL  size_in_bytes                                    │
//! ├──────────────────────────────────────────────────────┤
//! │ N × ClassRecord                                      │
//! ├──────────────────────────────────────────────────────┤
//! │ RS unknown (always 0 in R14, padding bits in R2000)  │
//! ├──────────────────────────────────────────────────────┤
//! │ CRC-X25                                              │
//! ├──────────────────────────────────────────────────────┤
//! │ End sentinel (16 bytes — CLASSES_END)                │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! Each `ClassRecord` is:
//!
//! ```text
//! BS class_number   (≥ 500 for custom classes; < 500 reserved)
//! BS version        (proxy/object version flags)
//! TV app_name       ("ObjectDBX Classes")
//! TV cpp_class_name ("AcDbHatch")
//! TV dxf_record_name ("HATCH")
//! B  was_zombie     (R14+: did the class load as zombie?)
//! BS item_class_id  (0x1F2 for entity, 0x1F3 for object)
//! ```
//!
//! For the DXF subset we round-trip (LINE, ARC, CIRCLE, LWPOLYLINE,
//! ELLIPSE, SPLINE, TEXT, MTEXT, INSERT, HATCH, plus the symbol-table
//! records LAYER / BLOCK_RECORD / STYLE / DIMSTYLE / LTYPE), every
//! class number is one of the fixed reserved IDs and the classes
//! section is empty. We still emit the section framing because R2000
//! readers require it.

use crate::dwg::bits::{crc_x25, BitReader, BitWriter};
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::sentinels::{CLASSES_BEGIN, CLASSES_END};
use crate::dwg::version::Version;

/// One class record. The DXF subset we round-trip uses only fixed
/// classes (LINE, CIRCLE, …), so this is needed only when a file
/// references custom AcDbProxyEntity-derived classes — which we
/// preserve verbatim from input to output but do not interpret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRecord {
    pub class_number: i32,
    pub version: i32,
    pub app_name: String,
    pub cpp_class_name: String,
    pub dxf_record_name: String,
    pub was_zombie: bool,
    pub item_class_id: i32,
}

/// Classes section, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassesSection {
    pub version: Version,
    pub classes: Vec<ClassRecord>,
}

impl ClassesSection {
    /// Build an empty section. Sufficient for any DWG that uses only
    /// the fixed classes.
    pub fn empty(version: Version) -> Self {
        Self {
            version,
            classes: Vec::new(),
        }
    }

    /// Encode the section into `out` (start sentinel + body + end sentinel).
    pub fn encode(&self, out: &mut Vec<u8>) -> DwgResult<()> {
        // Build the bit-encoded body in a scratch BitWriter, then frame
        // it with the size prefix and CRC.
        let mut body = BitWriter::new();
        // size_in_bytes placeholder (we'll fix once we know it).
        let size_placeholder_off = body.bit_position();
        body.write_rl(0)?;
        for c in &self.classes {
            body.write_bs(c.class_number)?;
            // BS in DWG is i32; class.version stored as i32 in our model.
            body.write_bs(c.version)?;
            body.write_tv(&c.app_name)?;
            body.write_tv(&c.cpp_class_name)?;
            body.write_tv(&c.dxf_record_name)?;
            body.write_b(c.was_zombie)?;
            body.write_bs(c.item_class_id)?;
        }
        // The classes section is byte-padded to the next byte boundary
        // by writing zero bits, then a final RS = 0 (R14+).
        body.write_rs(0)?;
        let bits = body.into_bytes();
        // Backfill size_in_bytes (the size from after the size field
        // itself to just before the CRC). For the empty section this
        // is just the trailing RS = 0, i.e. 2 bytes.
        let _ = size_placeholder_off;
        let mut framed = bits.clone();
        // Compute the actual body size_in_bytes: total bytes - 4 (the
        // RL prefix itself).
        let body_size = (framed.len() - 4) as u32;
        framed[0..4].copy_from_slice(&body_size.to_le_bytes());

        let crc = crc_x25(0xc0c1, &framed);
        framed.extend_from_slice(&crc.to_le_bytes());

        out.extend_from_slice(&CLASSES_BEGIN);
        out.extend_from_slice(&framed);
        out.extend_from_slice(&CLASSES_END);
        Ok(())
    }

    /// Parse the section starting at `offset`.
    pub fn parse(version: Version, bytes: &[u8], offset: usize) -> DwgResult<Self> {
        if bytes.len() < offset + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let begin = &bytes[offset..offset + 16];
        if begin != CLASSES_BEGIN.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(begin);
            return Err(DwgError::InvalidSentinel {
                section: "classes",
                expected: CLASSES_BEGIN,
                got,
            });
        }
        let after_begin = offset + 16;
        if bytes.len() < after_begin + 4 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let size_in_bytes = u32::from_le_bytes([
            bytes[after_begin],
            bytes[after_begin + 1],
            bytes[after_begin + 2],
            bytes[after_begin + 3],
        ]) as usize;
        // Body region (bit-encoded): 4 (size prefix) .. 4 + size_in_bytes.
        // CRC immediately follows: 2 bytes.
        // End sentinel: 16 bytes.
        let body_end = after_begin + 4 + size_in_bytes;
        if bytes.len() < body_end + 2 + 16 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let stored_crc = u16::from_le_bytes([bytes[body_end], bytes[body_end + 1]]);
        let computed_crc = crc_x25(0xc0c1, &bytes[after_begin..body_end]);
        if stored_crc != computed_crc {
            return Err(DwgError::SectionCrcMismatch {
                section: "classes",
                computed: u32::from(computed_crc),
                stored: u32::from(stored_crc),
            });
        }
        let end_off = body_end + 2;
        let end = &bytes[end_off..end_off + 16];
        if end != CLASSES_END.as_slice() {
            let mut got = [0u8; 16];
            got.copy_from_slice(end);
            return Err(DwgError::InvalidSentinel {
                section: "classes",
                expected: CLASSES_END,
                got,
            });
        }
        // Now decode the class records from the bit-encoded body.
        let mut reader = BitReader::new(&bytes[after_begin + 4..body_end]);
        let mut classes = Vec::new();
        // Heuristic: each record is at minimum ~5 bytes (two BS + three
        // empty TVs + 1 B + 1 BS). When fewer than 16 bits remain we
        // stop. The format does not encode a count; readers walk until
        // they exhaust the size prefix.
        while reader.remaining_bits() >= 16 {
            // Tentatively decode one record; if it errors, we treat the
            // remaining bits as the trailing RS + padding and stop.
            let snapshot = reader.bit_position();
            if let Ok(c) = decode_class_record(&mut reader) {
                classes.push(c);
            } else {
                reader.set_bit_position(snapshot)?;
                break;
            }
        }
        Ok(Self { version, classes })
    }
}

fn decode_class_record(r: &mut BitReader<'_>) -> DwgResult<ClassRecord> {
    Ok(ClassRecord {
        class_number: r.read_bs()?,
        version: r.read_bs()?,
        app_name: r.read_tv()?,
        cpp_class_name: r.read_tv()?,
        dxf_record_name: r.read_tv()?,
        was_zombie: r.read_b()?,
        item_class_id: r.read_bs()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_section_round_trips() {
        let section = ClassesSection::empty(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        let parsed = ClassesSection::parse(Version::R2000, &buf, 0).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn parse_rejects_wrong_sentinel() {
        let buf = vec![0u8; 64];
        assert!(matches!(
            ClassesSection::parse(Version::R2000, &buf, 0),
            Err(DwgError::InvalidSentinel { .. })
        ));
    }

    #[test]
    fn parse_rejects_crc_mismatch() {
        let section = ClassesSection::empty(Version::R2000);
        let mut buf = Vec::new();
        section.encode(&mut buf).unwrap();
        // Corrupt the body (between the size prefix and CRC).
        buf[16 + 5] ^= 0xff;
        let err = ClassesSection::parse(Version::R2000, &buf, 0).unwrap_err();
        assert!(matches!(err, DwgError::SectionCrcMismatch { .. }));
    }
}
