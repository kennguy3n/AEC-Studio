//! R13-R2000 object map (a.k.a. "Object Map / Handles section").
//!
//! The object map is an index of every object in the file, keyed by
//! handle. AutoCAD seeks objects by reading this section first, then
//! jumping to each object's recorded offset.
//!
//! On-disk layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ Object-map "pages" — each at most 2032 bytes:        │
//! │   RS  size (big-endian, includes the size bytes)     │
//! │   {                                                  │
//! │     MC handle_delta   (signed, from prev page entry) │
//! │     MC offset_delta   (signed, from prev page entry) │
//! │   }+                                                 │
//! │   CRC-X25 (2 bytes, big-endian)                      │
//! ├──────────────────────────────────────────────────────┤
//! │ Terminator page: RS = 2, CRC over [00 02] only       │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! Note: the RS size and CRC are **big-endian** in this section
//! specifically — DWG is otherwise little-endian throughout. This is a
//! known quirk documented in the OpenDesign Specification and verified
//! against LibreDWG fixtures.
//!
//! For files with zero objects (or files that locate objects via the
//! handle resolver only), this section reduces to the terminator page.

use crate::dwg::bits::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};
use std::collections::{BTreeMap, BTreeSet};

/// One entry in the object map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectMapEntry {
    /// Handle value (file-wide unique object identifier).
    pub handle: u64,
    /// Byte offset of the object record, interpreted relative to the
    /// containing OBJECTS section:
    ///
    /// - **R13 / R14 / R2000**: file-absolute offset (the OBJECTS
    ///   section is stored uncompressed at a known file offset, so the
    ///   absolute file offset and the section-relative offset are
    ///   numerically identical for AutoCAD's seek-and-decode loop).
    /// - **R2004+**: offset within the DECOMPRESSED OBJECTS section.
    ///   The on-disk OBJECTS page is LZ77-compressed, so a file-
    ///   absolute offset would be meaningless — external tools (e.g.
    ///   AutoCAD recovery) decompress the page into a flat buffer and
    ///   then seek into it using these offsets.
    pub file_offset: u64,
}

/// The entire object map (flat list of entries; pagination is an
/// encoding concern handled by [`Self::encode`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectMap {
    pub entries: Vec<ObjectMapEntry>,
}

impl ObjectMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Encode the map into `out`. Entries are written in the order
    /// supplied; callers that need them in handle order should sort
    /// beforehand.
    ///
    /// Emits content pages (up to 2032 bytes each) followed by a
    /// terminator page. A single entry must fit in a content page; the
    /// MC delta encoding caps each at ~9 bytes, so the 2032-byte cap
    /// gives ~225 entries per page — comfortably more than enough.
    pub fn encode(&self, out: &mut Vec<u8>) -> DwgResult<()> {
        const MAX_PAGE_BYTES: usize = 2032;
        let mut idx = 0;
        let mut last_handle: u64 = 0;
        let mut last_offset: u64 = 0;

        while idx < self.entries.len() {
            let mut body: Vec<u8> = Vec::new();
            while idx < self.entries.len() {
                let entry = &self.entries[idx];
                let h_delta = (entry.handle as i64).wrapping_sub(last_handle as i64);
                let o_delta = (entry.file_offset as i64).wrapping_sub(last_offset as i64);
                let mut entry_bytes = Vec::new();
                write_mc_i64(&mut entry_bytes, h_delta);
                write_mc_i64(&mut entry_bytes, o_delta);
                // Reserve 2 bytes for the size prefix + 2 for the CRC
                // when checking if this page is full.
                if body.len() + entry_bytes.len() + 2 + 2 > MAX_PAGE_BYTES {
                    break;
                }
                body.extend_from_slice(&entry_bytes);
                last_handle = entry.handle;
                last_offset = entry.file_offset;
                idx += 1;
            }
            if body.is_empty() {
                return Err(DwgError::InternalInvariant(
                    "single object-map entry exceeds 2032-byte page cap".to_string(),
                ));
            }
            let size = (body.len() + 2) as u16;
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&body);
            let mut crc_input = Vec::with_capacity(2 + body.len());
            crc_input.extend_from_slice(&size.to_be_bytes());
            crc_input.extend_from_slice(&body);
            let crc = crc_x25(0xc0c1, &crc_input);
            out.extend_from_slice(&crc.to_be_bytes());
        }

        // Terminator page: size = 2, body = empty, CRC over [00, 02].
        let term_size: u16 = 2;
        out.extend_from_slice(&term_size.to_be_bytes());
        let term_crc = crc_x25(0xc0c1, &term_size.to_be_bytes());
        out.extend_from_slice(&term_crc.to_be_bytes());
        Ok(())
    }

    /// Parse an object map from `bytes` starting at offset 0.
    pub fn parse(bytes: &[u8]) -> DwgResult<Self> {
        let mut cursor = 0usize;
        let mut entries = Vec::new();
        let mut last_handle: u64 = 0;
        let mut last_offset: u64 = 0;
        loop {
            if cursor + 2 > bytes.len() {
                return Err(DwgError::UnexpectedEof {
                    byte: bytes.len(),
                    bit: 0,
                });
            }
            let size = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
            if size < 2 {
                return Err(DwgError::MalformedObject {
                    class: "ObjectMap".to_string(),
                    offset: cursor as u64,
                    message: format!("page declares size < 2 (got {size})"),
                });
            }
            let page_end = cursor + size as usize;
            if page_end + 2 > bytes.len() {
                return Err(DwgError::UnexpectedEof {
                    byte: bytes.len(),
                    bit: 0,
                });
            }
            let crc_stored = u16::from_be_bytes([bytes[page_end], bytes[page_end + 1]]);
            let crc_computed = crc_x25(0xc0c1, &bytes[cursor..page_end]);
            if crc_stored != crc_computed {
                return Err(DwgError::SectionCrcMismatch {
                    section: "object_map_page",
                    computed: u32::from(crc_computed),
                    stored: u32::from(crc_stored),
                });
            }
            if size == 2 {
                // Terminator page.
                return Ok(Self { entries });
            }
            // Decode entries in this page.
            let body = &bytes[cursor + 2..page_end];
            let mut body_off = 0usize;
            while body_off < body.len() {
                let (h_delta, n1) = read_mc_i64(&body[body_off..])?;
                body_off += n1;
                let (o_delta, n2) = read_mc_i64(&body[body_off..])?;
                body_off += n2;
                last_handle = (last_handle as i64).wrapping_add(h_delta) as u64;
                last_offset = (last_offset as i64).wrapping_add(o_delta) as u64;
                entries.push(ObjectMapEntry {
                    handle: last_handle,
                    file_offset: last_offset,
                });
            }
            cursor = page_end + 2;
        }
    }

    /// Cross-check that every record handle is present in the
    /// object map (and vice versa), and that neither side contains
    /// duplicate handles.
    ///
    /// Used by the R2004/R2007 parsers as a defense-in-depth
    /// check after [`crate::dwg::file::objects_section::recover_objects_sequential`]:
    /// the records were walked from a flat payload, and a
    /// well-formed file's `AcDb:Handles` page MUST list every
    /// record handle exactly once (LibreDWG's
    /// `read_2007_section_handles` relies on this for the
    /// post-decode `dwg_resolve_handle` pass; two map entries with
    /// the same handle would race on which offset wins).
    ///
    /// Returns `DwgError::MalformedObject` for:
    /// - A duplicate handle inside the object map (two entries
    ///   pointing at the same handle, possibly different offsets).
    /// - A duplicate handle inside the recovered records (two
    ///   records claiming the same handle).
    /// - A record handle absent from the map (the map is the
    ///   canonical lookup side; a missing entry indicates a
    ///   corrupted handles section).
    /// - A map entry whose handle no record claims (orphaned map
    ///   entry).
    pub fn validate_against_records(
        &self,
        records: &[crate::dwg::file::r2000_layout::R2000Object],
    ) -> DwgResult<()> {
        let mut map_handles: BTreeMap<u64, u64> = BTreeMap::new();
        for entry in &self.entries {
            if let Some(prev_offset) = map_handles.insert(entry.handle, entry.file_offset) {
                return Err(DwgError::MalformedObject {
                    class: "ObjectMap".into(),
                    offset: entry.file_offset,
                    message: format!(
                        "duplicate handle {:#x} in object map (previously at \
                         offset {:#x}, now at offset {:#x}); a handle must \
                         resolve to exactly one record",
                        entry.handle, prev_offset, entry.file_offset
                    ),
                });
            }
        }
        let mut record_handles: BTreeSet<u64> = BTreeSet::new();
        for record in records {
            if !record_handles.insert(record.record_handle.value) {
                return Err(DwgError::MalformedObject {
                    class: "ObjectMap".into(),
                    offset: 0,
                    message: format!(
                        "duplicate record handle {:#x} in recovered OBJECTS \
                         payload; each record must claim a unique handle",
                        record.record_handle.value
                    ),
                });
            }
            if !map_handles.contains_key(&record.record_handle.value) {
                return Err(DwgError::MalformedObject {
                    class: "ObjectMap".into(),
                    offset: 0,
                    message: format!(
                        "record handle {:#x} is missing from the object map \
                         (map has {} entries; expected every record handle to \
                         appear)",
                        record.record_handle.value,
                        self.entries.len()
                    ),
                });
            }
        }
        for entry in &self.entries {
            if !record_handles.contains(&entry.handle) {
                return Err(DwgError::MalformedObject {
                    class: "ObjectMap".into(),
                    offset: entry.file_offset,
                    message: format!(
                        "object map entry references handle {:#x} but no record \
                         with that handle was recovered from the OBJECTS payload",
                        entry.handle
                    ),
                });
            }
        }
        Ok(())
    }
}

/// MC encoding (7-bit little-endian chunks) for a signed value.
///
/// Bit layout per byte:
///
/// - Continuation byte (all but the last): bit 7 = 1, bits 0..=6 hold
///   7 magnitude bits.
/// - Terminal byte (last): bit 7 = 0, bit 6 = sign, bits 0..=5 hold 6
///   magnitude bits.
///
/// Because the terminal byte only has 6 magnitude bits, if the highest
/// non-zero 7-bit chunk has bit 6 set we must push an extra zero
/// terminal chunk so the magnitude bit can't be misinterpreted as the
/// sign bit on decode. This mirrors LibreDWG's `bit_write_MC`.
fn write_mc_i64(out: &mut Vec<u8>, value: i64) {
    let negative = value < 0;
    let mut magnitude = value.unsigned_abs();
    let mut chunks: Vec<u8> = Vec::new();
    loop {
        let chunk = (magnitude & 0x7f) as u8;
        magnitude >>= 7;
        chunks.push(chunk);
        if magnitude == 0 {
            break;
        }
    }
    // Reserve bit 6 of the terminal byte for the sign by pushing an
    // empty terminal chunk when the high non-zero chunk uses bit 6.
    if let Some(last) = chunks.last() {
        if (*last & 0x40) != 0 {
            chunks.push(0);
        }
    }
    let n = chunks.len();
    for (i, chunk) in chunks.iter_mut().enumerate() {
        if i != n - 1 {
            *chunk |= 0x80; // continuation
        } else if negative {
            *chunk |= 0x40; // sign bit in terminal byte
        }
    }
    out.extend_from_slice(&chunks);
}

fn read_mc_i64(bytes: &[u8]) -> DwgResult<(i64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    let mut consumed = 0usize;
    loop {
        if consumed >= bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: consumed,
                bit: 0,
            });
        }
        let b = bytes[consumed];
        consumed += 1;
        if (b & 0x80) != 0 {
            // Continuation byte: 7 magnitude bits.
            value |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if shift >= 64 {
                return Err(DwgError::MalformedObject {
                    class: "MC".to_string(),
                    offset: consumed as u64,
                    message: format!("encoding overflows i64 (shift={shift})"),
                });
            }
        } else {
            // Terminal byte: bit 6 is sign, bits 0..=5 are magnitude.
            let negative = (b & 0x40) != 0;
            value |= u64::from(b & 0x3f) << shift;
            let signed = if negative {
                -(value as i64)
            } else {
                value as i64
            };
            return Ok((signed, consumed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_round_trips() {
        let map = ObjectMap::new();
        let mut buf = Vec::new();
        map.encode(&mut buf).unwrap();
        let parsed = ObjectMap::parse(&buf).unwrap();
        assert_eq!(parsed, map);
    }

    #[test]
    fn small_map_round_trips() {
        let map = ObjectMap {
            entries: vec![
                ObjectMapEntry {
                    handle: 0x10,
                    file_offset: 0x100,
                },
                ObjectMapEntry {
                    handle: 0x11,
                    file_offset: 0x120,
                },
                ObjectMapEntry {
                    handle: 0x21,
                    file_offset: 0x180,
                },
            ],
        };
        let mut buf = Vec::new();
        map.encode(&mut buf).unwrap();
        let parsed = ObjectMap::parse(&buf).unwrap();
        assert_eq!(parsed, map);
    }

    #[test]
    fn parse_rejects_crc_mismatch() {
        let map = ObjectMap {
            entries: vec![ObjectMapEntry {
                handle: 1,
                file_offset: 100,
            }],
        };
        let mut buf = Vec::new();
        map.encode(&mut buf).unwrap();
        // Corrupt body byte.
        buf[3] ^= 0xff;
        assert!(matches!(
            ObjectMap::parse(&buf),
            Err(DwgError::SectionCrcMismatch { .. })
        ));
    }

    #[test]
    fn mc_round_trip_positive() {
        for v in [0i64, 1, 63, 127, 128, 1_000, 1_000_000, i64::MAX / 2] {
            let mut buf = Vec::new();
            write_mc_i64(&mut buf, v);
            let (decoded, _) = read_mc_i64(&buf).unwrap();
            assert_eq!(decoded, v, "round-trip failure for {v}");
        }
    }

    #[test]
    fn mc_round_trip_negative() {
        for v in [-1i64, -63, -127, -128, -1_000, -1_000_000] {
            let mut buf = Vec::new();
            write_mc_i64(&mut buf, v);
            let (decoded, _) = read_mc_i64(&buf).unwrap();
            assert_eq!(decoded, v, "round-trip failure for {v}");
        }
    }

    mod validation {
        use super::*;
        use crate::dwg::bits::reader::HandleRef;
        use crate::dwg::entities::header_codec::CommonHeaderData;
        use crate::dwg::entities::ObjectType;
        use crate::dwg::file::r2000_layout::R2000Object;

        fn record(handle_value: u64) -> R2000Object {
            R2000Object {
                map_handle: handle_value,
                object_type: ObjectType::Line,
                record_handle: HandleRef {
                    code: 0,
                    value: handle_value,
                },
                common: CommonHeaderData::default(),
                raw_bytes: Vec::new(),
            }
        }

        fn entry(handle: u64, file_offset: u64) -> ObjectMapEntry {
            ObjectMapEntry {
                handle,
                file_offset,
            }
        }

        #[test]
        fn matching_handles_validate() {
            let map = ObjectMap {
                entries: vec![entry(0x10, 0x100), entry(0x20, 0x200)],
            };
            map.validate_against_records(&[record(0x10), record(0x20)])
                .unwrap();
        }

        #[test]
        fn duplicate_map_handle_is_rejected() {
            let map = ObjectMap {
                entries: vec![entry(0x10, 0x100), entry(0x10, 0x200)],
            };
            match map.validate_against_records(&[record(0x10)]) {
                Err(DwgError::MalformedObject { message, .. }) => {
                    assert!(
                        message.contains("duplicate handle 0x10 in object map"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected MalformedObject for duplicate map handle, got {other:?}"),
            }
        }

        #[test]
        fn duplicate_record_handle_is_rejected() {
            let map = ObjectMap {
                entries: vec![entry(0x10, 0x100)],
            };
            match map.validate_against_records(&[record(0x10), record(0x10)]) {
                Err(DwgError::MalformedObject { message, .. }) => {
                    assert!(
                        message.contains("duplicate record handle 0x10"),
                        "unexpected message: {message}"
                    );
                }
                other => {
                    panic!("expected MalformedObject for duplicate record handle, got {other:?}")
                }
            }
        }

        #[test]
        fn record_missing_from_map_is_rejected() {
            let map = ObjectMap {
                entries: vec![entry(0x10, 0x100)],
            };
            match map.validate_against_records(&[record(0x10), record(0x20)]) {
                Err(DwgError::MalformedObject { message, .. }) => {
                    assert!(
                        message.contains("record handle 0x20 is missing from the object map"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected MalformedObject for missing map entry, got {other:?}"),
            }
        }

        #[test]
        fn orphan_map_entry_is_rejected() {
            let map = ObjectMap {
                entries: vec![entry(0x10, 0x100), entry(0x20, 0x200)],
            };
            match map.validate_against_records(&[record(0x10)]) {
                Err(DwgError::MalformedObject { message, .. }) => {
                    assert!(
                        message.contains("references handle 0x20 but no record"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected MalformedObject for orphan map entry, got {other:?}"),
            }
        }
    }
}
