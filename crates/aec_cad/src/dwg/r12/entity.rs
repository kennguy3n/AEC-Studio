//! R12 (AC1009) fixed-record entity codec.
//!
//! Unlike the bit-encoded R13+ object records, R12 entities are
//! byte-aligned fixed-record blocks. Each record has this on-wire
//! shape (matching the AC1009 / pre-R13 layout LibreDWG decodes in
//! `decode.c::decode_preR13_entities`):
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ RC  type            — entity opcode, 1..=24 (see [`R12EntityType`])│
//! │ RS  size            — total record byte length (incl. type, excl. │
//! │                       trailing record CRC)                         │
//! │ RS  layer_index     — 1-based index into the LAYER table           │
//! │ RS  opts_r11        — per-entity-type bitmap of optional fields    │
//! │ RC  flag_r11        — common flag byte:                            │
//! │                         0x01 HAS_COLOR                             │
//! │                         0x02 HAS_LTYPE                             │
//! │                         0x04 HAS_ELEVATION                         │
//! │                         0x08 HAS_THICKNESS                         │
//! │                         0x20 HAS_HANDLING                          │
//! │                         0x40 HAS_PSPACE                            │
//! │                         0x80 HAS_ATTRIBS                           │
//! │                                                                    │
//! │ [if HAS_PSPACE]   RC extra_r11                                     │
//! │ [if HAS_COLOR]    RCs color  (-1=ByLayer, -2=ByBlock, 0..=255)     │
//! │ [if HAS_LTYPE]    RS ltype_index                                   │
//! │ [if HAS_ELEVATION] RD elevation  (skipped for LINE/POINT/3DFACE)   │
//! │ [if HAS_THICKNESS] RD thickness                                    │
//! │ [if HAS_HANDLING] RC handle_size + handle_size bytes               │
//! │                                                                    │
//! │ <entity-specific fixed-record fields>                              │
//! ├──────────────────────────────────────────────────────────────────┤
//! │ RS  crc16  — CRC-X25 over [type ..= last-byte-of-entity-data]      │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The `size` field is essential for the file walker: it tells the
//! walker exactly how many bytes belong to this record, including any
//! optional appendices and the trailing CRC. The walker does NOT need
//! per-type knowledge to step past unknown record types; it can skip
//! `size` bytes from the start of the type opcode and continue.
//!
//! Reference: LibreDWG's `common_entity_data.spec` (PRE(R_13b1) branch)
//! and `decode.c::decode_preR13_entities`. The opcodes are documented
//! in `include/dwg.h` as the `Dwg_Object_Type_r11` enum.

use crate::dwg::bits::crc::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};

/// AC1009 entity opcodes (1..=24). Values from LibreDWG's
/// `Dwg_Object_Type_r11`; matches the AutoCAD R12 binary spec.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R12EntityType {
    Line = 1,
    Point = 2,
    Circle = 3,
    Shape = 4,
    Repeat = 5,
    EndRep = 6,
    Text = 7,
    Arc = 8,
    Trace = 9,
    Load = 10,
    Solid = 11,
    Block = 12,
    EndBlk = 13,
    Insert = 14,
    Attdef = 15,
    Attrib = 16,
    SeqEnd = 17,
    Jump = 18,
    Polyline = 19,
    Vertex = 20,
    /// "3D-line" — vestigial pre-R10 LINE-with-Z; not emitted today
    /// but recognised so we can skip records cleanly.
    Line3D = 21,
    Face3D = 22,
    Dimension = 23,
    Viewport = 24,
}

impl R12EntityType {
    /// Recover the typed enum from the on-wire opcode byte. Returns
    /// `None` for opcodes that aren't part of the documented R12 set
    /// so the caller can surface a structured error rather than fall
    /// through to a "default" decoder that would misalign the cursor.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Line,
            2 => Self::Point,
            3 => Self::Circle,
            4 => Self::Shape,
            5 => Self::Repeat,
            6 => Self::EndRep,
            7 => Self::Text,
            8 => Self::Arc,
            9 => Self::Trace,
            10 => Self::Load,
            11 => Self::Solid,
            12 => Self::Block,
            13 => Self::EndBlk,
            14 => Self::Insert,
            15 => Self::Attdef,
            16 => Self::Attrib,
            17 => Self::SeqEnd,
            18 => Self::Jump,
            19 => Self::Polyline,
            20 => Self::Vertex,
            21 => Self::Line3D,
            22 => Self::Face3D,
            23 => Self::Dimension,
            24 => Self::Viewport,
            _ => return None,
        })
    }
}

/// Common-flag byte (`flag_r11`) bitfield. Each bit means "this
/// optional field is present in the appendix block immediately
/// following the entity-specific data".
pub mod common_flag {
    pub const HAS_COLOR: u8 = 0x01;
    pub const HAS_LTYPE: u8 = 0x02;
    pub const HAS_ELEVATION: u8 = 0x04;
    pub const HAS_THICKNESS: u8 = 0x08;
    pub const HAS_HANDLING: u8 = 0x20;
    pub const HAS_PSPACE: u8 = 0x40;
    pub const HAS_ATTRIBS: u8 = 0x80;
}

/// Common header recovered from every R12 entity record.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct R12EntityCommon {
    /// 1-based index into the LAYER table.
    pub layer_index: u16,
    /// Per-entity-type optional-field bitmap.
    pub opts: u16,
    /// Common flag byte (see [`common_flag`]).
    pub flag: u8,
    /// Paper-space discriminator (only present when `flag & HAS_PSPACE`).
    pub extra: u8,
    /// AutoCAD color index: -1=ByLayer, -2=ByBlock, 0..=255 = palette.
    /// Defaults to ByLayer when `HAS_COLOR` bit is clear.
    pub color: i16,
    /// LTYPE table index. Defaults to 0 (ByBlock-style "no override").
    pub ltype_index: u16,
    /// Elevation override (defaults to 0.0).
    pub elevation: f64,
    /// Thickness override (defaults to 0.0).
    pub thickness: f64,
    /// Entity handle bytes (variable-length). Empty when the file is
    /// pre-handles or when `HAS_HANDLING` bit is clear.
    pub handle: Vec<u8>,
}

impl R12EntityCommon {
    /// Build a defaulted common header for a given layer.
    pub fn on_layer(layer_index: u16) -> Self {
        Self {
            layer_index,
            opts: 0,
            flag: 0,
            extra: 0,
            color: -1,
            ltype_index: 0,
            elevation: 0.0,
            thickness: 0.0,
            handle: Vec::new(),
        }
    }
}

/// One fully decoded R12 entity record. The `payload` field carries
/// the raw bytes between the common appendix and the trailing CRC so
/// per-type codecs in [`record_kinds`] can hand back typed structs
/// without forcing the framing module to know every variant.
#[derive(Debug, Clone, PartialEq)]
pub struct R12EntityRecord {
    pub kind: R12EntityType,
    pub common: R12EntityCommon,
    pub payload: Vec<u8>,
}

/// Encode the common header + appendix block. Returns the byte stream
/// covering opcode..end-of-appendix (NOT the CRC). The caller is
/// responsible for appending the per-entity payload and computing the
/// final record CRC via [`finalize_record`].
fn encode_common(
    buf: &mut Vec<u8>,
    kind: R12EntityType,
    common: &R12EntityCommon,
) -> DwgResult<()> {
    // RC type
    buf.push(kind as u8);
    // RS size — back-patched at finalize time
    buf.extend_from_slice(&[0u8, 0]);
    // RS layer_index (1-based; index 0 is reserved)
    buf.extend_from_slice(&common.layer_index.to_le_bytes());
    // RS opts_r11
    buf.extend_from_slice(&common.opts.to_le_bytes());
    // RC flag_r11
    buf.push(common.flag);

    if common.flag & common_flag::HAS_PSPACE != 0 {
        buf.push(common.extra);
    }
    if common.flag & common_flag::HAS_COLOR != 0 {
        buf.extend_from_slice(&common.color.to_le_bytes());
    }
    if common.flag & common_flag::HAS_LTYPE != 0 {
        buf.extend_from_slice(&common.ltype_index.to_le_bytes());
    }
    let suppress_elevation = matches!(
        kind,
        R12EntityType::Line | R12EntityType::Point | R12EntityType::Face3D
    );
    if common.flag & common_flag::HAS_ELEVATION != 0 && !suppress_elevation {
        buf.extend_from_slice(&common.elevation.to_le_bytes());
    }
    if common.flag & common_flag::HAS_THICKNESS != 0 {
        buf.extend_from_slice(&common.thickness.to_le_bytes());
    }
    if common.flag & common_flag::HAS_HANDLING != 0 {
        if common.handle.len() > u8::MAX as usize {
            return Err(DwgError::InternalInvariant(format!(
                "R12 entity handle is {} bytes; per-spec max is 255",
                common.handle.len()
            )));
        }
        buf.push(common.handle.len() as u8);
        buf.extend_from_slice(&common.handle);
    }
    Ok(())
}

/// Decode the common header + appendix from `bytes`, starting at
/// offset 0. Returns the parsed common header, the kind opcode, the
/// declared record size, and the cursor position immediately after
/// the appendix (so the caller can pick up per-type payload bytes).
fn decode_common(bytes: &[u8]) -> DwgResult<(R12EntityType, R12EntityCommon, u16, usize)> {
    if bytes.len() < 8 {
        // Min: 1 (type) + 2 (size) + 2 (layer) + 2 (opts) + 1 (flag) = 8
        return Err(DwgError::UnexpectedEof { byte: 8, bit: 0 });
    }
    let kind_raw = bytes[0];
    // Per LibreDWG: a high bit (>= 0x80) on the opcode marks a
    // "deleted" record that has been moved into a BLOCK. We surface
    // that to the caller via the lossless opcode but strip the bit
    // here so the type table works.
    let kind_value = kind_raw & 0x7f;
    let kind = R12EntityType::from_u8(kind_value).ok_or_else(|| DwgError::MalformedObject {
        class: "R12EntityRecord".to_string(),
        offset: 0,
        message: format!("unknown opcode 0x{kind_raw:02x}"),
    })?;
    let size = u16::from_le_bytes([bytes[1], bytes[2]]);
    if (size as usize) < 8 || (size as usize) > bytes.len() {
        return Err(DwgError::MalformedObject {
            class: format!("{kind:?}"),
            offset: 0,
            message: format!(
                "declared size {size} out of range (have {} bytes available, min 8)",
                bytes.len()
            ),
        });
    }
    let layer_index = u16::from_le_bytes([bytes[3], bytes[4]]);
    let opts = u16::from_le_bytes([bytes[5], bytes[6]]);
    let flag = bytes[7];

    let mut cur = 8usize;
    let mut extra = 0u8;
    let mut color: i16 = -1;
    let mut ltype_index: u16 = 0;
    let mut elevation: f64 = 0.0;
    let mut thickness: f64 = 0.0;
    let mut handle: Vec<u8> = Vec::new();

    let need = |cur: usize, want: usize, bytes: &[u8]| -> DwgResult<()> {
        if cur + want > bytes.len() {
            Err(DwgError::UnexpectedEof {
                byte: cur + want,
                bit: 0,
            })
        } else {
            Ok(())
        }
    };

    if flag & common_flag::HAS_PSPACE != 0 {
        need(cur, 1, bytes)?;
        extra = bytes[cur];
        cur += 1;
    }
    if flag & common_flag::HAS_COLOR != 0 {
        need(cur, 2, bytes)?;
        color = i16::from_le_bytes([bytes[cur], bytes[cur + 1]]);
        cur += 2;
    }
    if flag & common_flag::HAS_LTYPE != 0 {
        need(cur, 2, bytes)?;
        ltype_index = u16::from_le_bytes([bytes[cur], bytes[cur + 1]]);
        cur += 2;
    }
    let suppress_elevation = matches!(
        kind,
        R12EntityType::Line | R12EntityType::Point | R12EntityType::Face3D
    );
    if flag & common_flag::HAS_ELEVATION != 0 && !suppress_elevation {
        need(cur, 8, bytes)?;
        let mut z = [0u8; 8];
        z.copy_from_slice(&bytes[cur..cur + 8]);
        elevation = f64::from_le_bytes(z);
        cur += 8;
    }
    if flag & common_flag::HAS_THICKNESS != 0 {
        need(cur, 8, bytes)?;
        let mut z = [0u8; 8];
        z.copy_from_slice(&bytes[cur..cur + 8]);
        thickness = f64::from_le_bytes(z);
        cur += 8;
    }
    if flag & common_flag::HAS_HANDLING != 0 {
        need(cur, 1, bytes)?;
        let len = bytes[cur] as usize;
        cur += 1;
        need(cur, len, bytes)?;
        handle.extend_from_slice(&bytes[cur..cur + len]);
        cur += len;
    }

    Ok((
        kind,
        R12EntityCommon {
            layer_index,
            opts,
            flag,
            extra,
            color,
            ltype_index,
            elevation,
            thickness,
            handle,
        },
        size,
        cur,
    ))
}

/// Patch the size field and append the trailing CRC-X25 to a record
/// whose payload bytes have already been written into `buf` starting
/// at `record_start`.
fn finalize_record(buf: &mut Vec<u8>, record_start: usize) -> DwgResult<()> {
    let total_inner = buf.len() - record_start;
    if total_inner > u16::MAX as usize - 2 {
        return Err(DwgError::WriteOverflow {
            limit: u16::MAX as usize - 2,
        });
    }
    let size_value = total_inner as u16;
    buf[record_start + 1] = size_value.to_le_bytes()[0];
    buf[record_start + 2] = size_value.to_le_bytes()[1];
    let crc = crc_x25(0xc0c1, &buf[record_start..]);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Write one complete R12 entity record (opcode → CRC) into `buf`.
pub fn encode_record(buf: &mut Vec<u8>, record: &R12EntityRecord) -> DwgResult<()> {
    let start = buf.len();
    encode_common(buf, record.kind, &record.common)?;
    buf.extend_from_slice(&record.payload);
    finalize_record(buf, start)
}

/// Decode one complete R12 entity record. Returns the record and the
/// number of bytes consumed (which equals `size + 2` for the trailing
/// CRC). Errors if the trailing CRC doesn't match.
pub fn decode_record(bytes: &[u8]) -> DwgResult<(R12EntityRecord, usize)> {
    let (kind, common, size, payload_offset) = decode_common(bytes)?;
    let payload_end = size as usize;
    if payload_end < payload_offset {
        return Err(DwgError::MalformedObject {
            class: format!("{kind:?}"),
            offset: 0,
            message: format!(
                "common appendix consumed {payload_offset} bytes but record size is {payload_end}"
            ),
        });
    }
    if bytes.len() < payload_end + 2 {
        return Err(DwgError::UnexpectedEof {
            byte: payload_end + 2,
            bit: 0,
        });
    }
    let payload = bytes[payload_offset..payload_end].to_vec();
    let stored_crc = u16::from_le_bytes([bytes[payload_end], bytes[payload_end + 1]]);
    let computed_crc = crc_x25(0xc0c1, &bytes[..payload_end]);
    if stored_crc != computed_crc {
        return Err(DwgError::SectionCrcMismatch {
            section: "r12_entity_record",
            computed: u32::from(computed_crc),
            stored: u32::from(stored_crc),
        });
    }
    Ok((
        R12EntityRecord {
            kind,
            common,
            payload,
        },
        payload_end + 2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_from_u8_covers_full_r12_table() {
        for op in 1u8..=24u8 {
            assert!(
                R12EntityType::from_u8(op).is_some(),
                "opcode {op} should be recognised"
            );
        }
        assert!(R12EntityType::from_u8(0).is_none());
        assert!(R12EntityType::from_u8(25).is_none());
        assert!(R12EntityType::from_u8(0xff).is_none());
    }

    #[test]
    fn common_round_trips_bare_minimum_record() {
        let record = R12EntityRecord {
            kind: R12EntityType::Line,
            common: R12EntityCommon::on_layer(1),
            payload: vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        };
        let mut buf = Vec::new();
        encode_record(&mut buf, &record).unwrap();
        let (decoded, consumed) = decode_record(&buf).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn common_round_trips_with_full_appendix_block() {
        let record = R12EntityRecord {
            kind: R12EntityType::Circle,
            common: R12EntityCommon {
                layer_index: 2,
                opts: 0x0001,
                flag: common_flag::HAS_PSPACE
                    | common_flag::HAS_COLOR
                    | common_flag::HAS_LTYPE
                    | common_flag::HAS_ELEVATION
                    | common_flag::HAS_THICKNESS
                    | common_flag::HAS_HANDLING,
                extra: 0x42,
                color: 7,
                ltype_index: 3,
                elevation: 1.5,
                thickness: -0.25,
                handle: vec![0xde, 0xad, 0xbe, 0xef],
            },
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let mut buf = Vec::new();
        encode_record(&mut buf, &record).unwrap();
        let (decoded, consumed) = decode_record(&buf).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn common_skips_elevation_for_line_point_3dface() {
        // For LINE/POINT/3DFACE the HAS_ELEVATION flag is still set
        // (so older readers see the flag) but the elevation field
        // itself is NOT written. The decoder mirrors that suppression.
        // Net effect: elevation reads back as 0.0 even if the
        // encoder's input said otherwise.
        for kind in [
            R12EntityType::Line,
            R12EntityType::Point,
            R12EntityType::Face3D,
        ] {
            let record = R12EntityRecord {
                kind,
                common: R12EntityCommon {
                    layer_index: 1,
                    opts: 0,
                    flag: common_flag::HAS_ELEVATION,
                    extra: 0,
                    color: -1,
                    ltype_index: 0,
                    elevation: 9.9, // value should be dropped on encode
                    thickness: 0.0,
                    handle: Vec::new(),
                },
                payload: Vec::new(),
            };
            let mut buf = Vec::new();
            encode_record(&mut buf, &record).unwrap();
            let (decoded, _) = decode_record(&buf).unwrap();
            assert_eq!(
                decoded.common.elevation, 0.0,
                "{kind:?} should suppress elevation"
            );
        }
    }

    #[test]
    fn decode_rejects_unknown_opcode() {
        let mut buf = vec![0u8; 32];
        buf[0] = 0x5a; // not in 1..=24
        buf[1..3].copy_from_slice(&8u16.to_le_bytes());
        let err = decode_record(&buf).unwrap_err();
        assert!(
            matches!(err, DwgError::MalformedObject { ref class, .. } if class == "R12EntityRecord"),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_crc_mismatch() {
        let record = R12EntityRecord {
            kind: R12EntityType::Arc,
            common: R12EntityCommon::on_layer(1),
            payload: vec![1; 16],
        };
        let mut buf = Vec::new();
        encode_record(&mut buf, &record).unwrap();
        let last = buf.len() - 1;
        buf[last] ^= 0xff;
        let err = decode_record(&buf).unwrap_err();
        assert!(
            matches!(err, DwgError::SectionCrcMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_short_buffer_in_appendix() {
        // Encode a valid record, then truncate just before the CRC to
        // hit the EOF path in decode_common's handle-block read.
        let record = R12EntityRecord {
            kind: R12EntityType::Text,
            common: R12EntityCommon {
                flag: common_flag::HAS_HANDLING,
                handle: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                ..R12EntityCommon::on_layer(1)
            },
            payload: Vec::new(),
        };
        let mut buf = Vec::new();
        encode_record(&mut buf, &record).unwrap();
        // Snip the last handle byte AND the CRC away.
        let truncated = &buf[..buf.len() - 3];
        let err = decode_record(truncated).unwrap_err();
        assert!(
            matches!(err, DwgError::UnexpectedEof { .. })
                || matches!(err, DwgError::MalformedObject { .. }),
            "got {err:?}"
        );
    }
}
