//! R2000 (and R14) full file layout — orchestrates the header,
//! section locators, section CRCs, and the on-disk positioning of
//! each section relative to file offsets.
//!
//! Layout produced by [`assemble_r2000`] (matches LibreDWG
//! `encode.c::dwg_encode_chains` for R13–R2000 with 3 sections):
//!
//! ```text
//! 0x00 │ File header (FIXED_HEADER_LEN = 0x19 bytes)
//! 0x19 │   + 3 section locator records (9 bytes each):
//!      │     [0] SECTION_HEADER_R13   → ofs_header,   size
//!      │     [1] SECTION_CLASSES_R13  → ofs_classes,  size
//!      │     [2] SECTION_HANDLES_R13  → ofs_handles,  size
//!      │   + locator-block CRC-X25 (2 bytes LE, seed 0xC0C1)
//!      │   + DWG_SENTINEL_HEADER_END (16 bytes)
//! ofs_header  │ HEADER_VARS section (VARIABLE_BEGIN/END bracketed)
//! ofs_classes │ CLASSES section (CLASS_BEGIN/END bracketed)
//! ofs_objs    │ Object records (concatenated, NO section locator)
//! ofs_handles │ HANDLES (object map) page sequence
//! ```
//!
//! Critical conformance points (cross-checked against LibreDWG
//! `decode.c::decode_R13_R2000` and `encode.c::dwg_encode_chains`):
//!
//! 1. The fixed file header is **0x19 bytes**, not 0x20. There is no
//!    3-byte padding between codepage (@ 0x13–0x14) and the section
//!    count (@ 0x15–0x18). LibreDWG asserts `dat->byte == 0x19`
//!    immediately before reading the first locator record.
//! 2. The locator-block CRC uses a **plain CRC-X25 with seed 0xC0C1**.
//!    The ODA "xor_section_CRC" table (with per-locator-count XOR
//!    constants) is documented in LibreDWG `decode.c` as a known ODA
//!    spec error — do not apply it.
//! 3. A **`DWG_SENTINEL_HEADER_END`** (16 bytes) must be written
//!    immediately after the locator CRC. LibreDWG forward-searches
//!    for this pattern to confirm the header block parsed cleanly.
//! 4. There is **no separate "Objects" section locator** in R13–R2000.
//!    Object records are written between the Classes section and the
//!    Handles map at arbitrary file offsets, and the Handles map
//!    (section 2) is what records each handle→offset mapping.
//! 5. The second-header sentinel block is **optional**. LibreDWG only
//!    decodes it if it finds `DWG_SENTINEL_2NDHEADER_BEGIN` via a
//!    forward search; omitting it is well-formed.

use crate::dwg::bits::crc_x25;
use crate::dwg::bits::reader::HandleRef;
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::ObjectRecord;
use crate::dwg::entities::ObjectType;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::classes::ClassesSection;
use crate::dwg::file::header::{FileHeader, FIXED_HEADER_LEN};
use crate::dwg::file::header_vars::HeaderVarsSection;
use crate::dwg::file::object_map::{ObjectMap, ObjectMapEntry};
use crate::dwg::file::sections::{encode_locators, parse_locators, SectionId, SectionLocator};
use crate::dwg::file::sentinels::HEADER_END;
use crate::dwg::version::Version;

/// Plain CRC-X25 seed used for every CRC LibreDWG computes in the
/// R13–R2000 header + locator block. See module docs point (2).
const HEADER_CRC_SEED: u16 = 0xC0C1;

/// Number of section locator records we emit. LibreDWG accepts
/// anywhere from 3 to 6; the canonical minimal layout is 3:
/// Header (0), Classes (1), Handles (2).
const LOCATOR_COUNT: usize = 3;

/// All sections needed to write a complete R14/R2000 file.
pub struct R2000FileParts {
    pub version: Version,
    pub header_vars: HeaderVarsSection,
    pub classes: ClassesSection,
    pub objects: Vec<ObjectRecord>,
}

/// Structural summary of one object record plus the raw wire bytes.
///
/// For R14/R2000 the payload-to-handle-stream boundary cannot be
/// determined without per-type knowledge (no `bitsize` field marker).
/// The file walker therefore peeks the structural header and stores
/// the raw bytes; a caller with a per-type decoder can recover the
/// full entity via [`ObjectRecord::decode_with`] on `raw_bytes`.
#[derive(Debug, Clone, PartialEq)]
pub struct R2000Object {
    /// Handle the object map indexed this record under (canonical).
    pub map_handle: u64,
    /// Object class id from the BS object_type field.
    pub object_type: ObjectType,
    /// The record's own handle (from H after BS type). Should match
    /// `map_handle` for well-formed files; we keep both to surface
    /// any inconsistency.
    pub record_handle: HandleRef,
    /// Decoded common entity header (entity_mode, layer, color, ...).
    pub common: CommonHeaderData,
    /// Total wire bytes (MS + body + CRC).
    pub raw_bytes: Vec<u8>,
}

/// Decoded view of a full R14/R2000 file in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct R2000File {
    pub version: Version,
    pub header_vars: HeaderVarsSection,
    pub classes: ClassesSection,
    /// Object record summaries plus raw wire bytes. Order matches
    /// the on-disk object-map order.
    pub objects: Vec<R2000Object>,
}

/// Number of bytes occupied by the file header + locator block +
/// locator CRC + post-CRC `HEADER_END` sentinel. This is the file
/// offset at which the first locator-addressable section data may
/// start.
///
/// Layout: [`FIXED_HEADER_LEN`] (0x19) + `9 * locator_count` +
/// 2 (CRC-X25) + 16 (`HEADER_END` sentinel).
fn header_block_size(locator_count: usize) -> usize {
    FIXED_HEADER_LEN + locator_count * 9 + 2 + HEADER_END.len()
}

/// Assemble a complete R14/R2000 file from its in-memory parts.
///
/// Strategy: lay out the sections in a fixed order, compute their
/// offsets and sizes, then back-patch the locator block. Object
/// records are written contiguously between the Classes section and
/// the Handles map, and the Handles map records each handle's file
/// offset.
pub fn assemble_r2000(parts: R2000FileParts) -> DwgResult<Vec<u8>> {
    if !matches!(parts.version, Version::R14 | Version::R2000) {
        return Err(DwgError::UnsupportedInVersion {
            version: parts.version,
            what: format!(
                "assemble_r2000 only supports R14/R2000; got {:?}",
                parts.version
            ),
        });
    }

    let prefix_size = header_block_size(LOCATOR_COUNT);

    // Encode each section into a freestanding buffer.
    let mut header_vars_bytes = Vec::new();
    parts.header_vars.encode(&mut header_vars_bytes);
    let mut classes_bytes = Vec::new();
    parts.classes.encode(&mut classes_bytes)?;

    // Object records are written between Classes and Handles.
    // Track each record's offset within the concatenated object
    // stream so we can patch absolute file offsets after we know
    // where the stream begins.
    let mut objects_bytes = Vec::new();
    let mut record_stream_offsets: Vec<u64> = Vec::with_capacity(parts.objects.len());
    for record in &parts.objects {
        record_stream_offsets.push(objects_bytes.len() as u64);
        let wire = record.encode(parts.version)?;
        objects_bytes.extend_from_slice(&wire);
    }

    // Compute file-absolute offsets.
    let mut cursor = prefix_size as u32;
    let header_offset = cursor;
    cursor += header_vars_bytes.len() as u32;
    let classes_offset = cursor;
    cursor += classes_bytes.len() as u32;
    let objects_offset = cursor;
    cursor += objects_bytes.len() as u32;
    let handles_offset = cursor;

    // Build the object map: entries sorted by handle, file offsets
    // resolved relative to the objects-stream start.
    let mut sorted_indices: Vec<usize> = (0..parts.objects.len()).collect();
    sorted_indices.sort_by_key(|&i| parts.objects[i].handle.value);
    let mut object_map = ObjectMap::new();
    for &i in &sorted_indices {
        object_map.entries.push(ObjectMapEntry {
            handle: parts.objects[i].handle.value,
            file_offset: objects_offset as u64 + record_stream_offsets[i],
        });
    }
    let mut handles_bytes = Vec::new();
    object_map.encode(&mut handles_bytes)?;
    let total_size = cursor + handles_bytes.len() as u32;

    // Build the locator records (only 3: Header, Classes, Handles).
    let locators = vec![
        SectionLocator {
            id: SectionId::Header,
            seeker: header_offset,
            size: header_vars_bytes.len() as u32,
        },
        SectionLocator {
            id: SectionId::Classes,
            seeker: classes_offset,
            size: classes_bytes.len() as u32,
        },
        SectionLocator {
            id: SectionId::Handles,
            seeker: handles_offset,
            size: handles_bytes.len() as u32,
        },
    ];
    let locator_bytes = encode_locators(&locators);
    debug_assert_eq!(locator_bytes.len(), LOCATOR_COUNT * 9);

    // Build the header.
    let header = FileHeader {
        version: parts.version,
        maintenance_release: 0,
        preview_offset: 0,
        codepage: 30,
        section_locator_count: LOCATOR_COUNT as u32,
    };
    let mut header_bytes = header.encode();
    debug_assert_eq!(header_bytes.len(), FIXED_HEADER_LEN);
    header_bytes.extend_from_slice(&locator_bytes);

    // CRC-X25 over the file header (0x19 bytes) + locator records.
    // Plain seed 0xC0C1 — no ODA per-count XOR.
    let crc = crc_x25(HEADER_CRC_SEED, &header_bytes);
    header_bytes.extend_from_slice(&crc.to_le_bytes());

    // HEADER_END sentinel.
    header_bytes.extend_from_slice(&HEADER_END);

    debug_assert_eq!(
        header_bytes.len(),
        prefix_size,
        "header block size {} != computed prefix {}",
        header_bytes.len(),
        prefix_size
    );

    let mut out = Vec::with_capacity(total_size as usize);
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&header_vars_bytes);
    out.extend_from_slice(&classes_bytes);
    out.extend_from_slice(&objects_bytes);
    out.extend_from_slice(&handles_bytes);

    Ok(out)
}

/// Parse a complete R14/R2000 file. Returns the decoded sections plus
/// the object-record stream (recovered via the Handles map).
pub fn parse_r2000(bytes: &[u8]) -> DwgResult<R2000File> {
    // Detect version from the signature.
    let version = crate::dwg::version::detect(bytes).ok_or({
        DwgError::InvalidSignature({
            let mut s = [0u8; 6];
            let n = bytes.len().min(6);
            s[..n].copy_from_slice(&bytes[..n]);
            s
        })
    })?;
    if !matches!(version, Version::R14 | Version::R2000) {
        return Err(DwgError::UnsupportedInVersion {
            version,
            what: "parse_r2000 only handles R14/R2000".into(),
        });
    }
    let header = FileHeader::parse(bytes, version)?;
    let locator_count = header.section_locator_count as usize;

    // Locators live at byte FIXED_HEADER_LEN (0x19).
    let locators = parse_locators(bytes, FIXED_HEADER_LEN, header.section_locator_count)?;

    // Verify the header CRC over [0..FIXED_HEADER_LEN + 9*count] with
    // plain seed 0xC0C1.
    let crc_offset = FIXED_HEADER_LEN + locator_count * 9;
    if bytes.len() < crc_offset + 2 + HEADER_END.len() {
        return Err(DwgError::UnexpectedEof {
            byte: crc_offset + 2 + HEADER_END.len(),
            bit: 0,
        });
    }
    let stored_crc = u16::from_le_bytes([bytes[crc_offset], bytes[crc_offset + 1]]);
    let computed_crc = crc_x25(HEADER_CRC_SEED, &bytes[..crc_offset]);
    if stored_crc != computed_crc {
        return Err(DwgError::HeaderCrcMismatch {
            computed: computed_crc,
            stored: stored_crc,
        });
    }

    // Verify the HEADER_END sentinel.
    let sentinel_offset = crc_offset + 2;
    let actual_sentinel = &bytes[sentinel_offset..sentinel_offset + HEADER_END.len()];
    if actual_sentinel != HEADER_END {
        return Err(DwgError::MalformedObject {
            class: "FileLayout".into(),
            offset: sentinel_offset as u64,
            message: "missing or wrong HEADER_END sentinel".into(),
        });
    }

    // Walk each well-known section.
    let mut header_vars: Option<HeaderVarsSection> = None;
    let mut classes: Option<ClassesSection> = None;
    let mut handles_range: Option<(usize, usize)> = None;
    for loc in &locators {
        let start = loc.seeker as usize;
        let end = start + loc.size as usize;
        if end > bytes.len() {
            return Err(DwgError::DanglingHandle {
                handle: 0,
                offset: loc.seeker as u64,
                file_size: bytes.len(),
            });
        }
        match loc.id {
            SectionId::Header => {
                header_vars = Some(HeaderVarsSection::parse(version, bytes, start)?);
            }
            SectionId::Classes => {
                classes = Some(ClassesSection::parse(version, bytes, start)?);
            }
            SectionId::Handles => {
                handles_range = Some((start, end));
            }
            // Optional sections we tolerate but don't structurally
            // decode yet.
            SectionId::ObjFreeSpace
            | SectionId::Template
            | SectionId::AuxHeader
            | SectionId::Thumbnail
            | SectionId::Unknown(_) => {}
        }
    }

    let header_vars = header_vars.ok_or_else(|| DwgError::MalformedObject {
        class: "FileLayout".into(),
        offset: 0,
        message: "no HEADER_VARS section locator".into(),
    })?;
    let classes = classes.ok_or_else(|| DwgError::MalformedObject {
        class: "FileLayout".into(),
        offset: 0,
        message: "no CLASSES section locator".into(),
    })?;
    let handles_range = handles_range.ok_or_else(|| DwgError::MalformedObject {
        class: "FileLayout".into(),
        offset: 0,
        message: "no HANDLES section locator".into(),
    })?;

    let object_map = ObjectMap::parse(&bytes[handles_range.0..handles_range.1])?;

    // For each object-map entry, peek the structural header (so we know
    // the record's wire size) and store the raw record bytes. We do
    // NOT eagerly call `ObjectRecord::decode` because for R14/R2000
    // that would mis-parse any record with a non-empty payload — the
    // payload-to-handle-stream boundary requires per-type knowledge
    // and `decode` has no version-specific bitsize marker to fall back
    // on. The caller invokes `ObjectRecord::decode_with(raw_bytes, ..)`
    // with a per-type decoder to recover the full entity.
    let mut objects = Vec::with_capacity(object_map.entries.len());
    for entry in &object_map.entries {
        let off = entry.file_offset as usize;
        if off >= bytes.len() {
            return Err(DwgError::DanglingHandle {
                handle: entry.handle,
                offset: entry.file_offset,
                file_size: bytes.len(),
            });
        }
        let (object_type, record_handle, common, total) =
            ObjectRecord::peek_header(version, &bytes[off..])?;
        if off + total > bytes.len() {
            return Err(DwgError::UnexpectedEof {
                byte: off + total,
                bit: 0,
            });
        }
        let raw_bytes = bytes[off..off + total].to_vec();
        objects.push(R2000Object {
            map_handle: entry.handle,
            object_type,
            record_handle,
            common,
            raw_bytes,
        });
    }

    Ok(R2000File {
        version,
        header_vars,
        classes,
        objects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::bits::reader::HandleRef;
    use crate::dwg::bits::BitWriter;
    use crate::dwg::entities::header_codec::CommonHeaderData;
    use crate::dwg::entities::line::LineEntity;
    use crate::dwg::entities::record::{BitBuf, ObjectHandles};
    use crate::dwg::entities::{ObjectRecord, ObjectType};

    fn one_line_record() -> ObjectRecord {
        let line = LineEntity {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 5.0, 0.0],
            thickness: 0.0,
            extrusion: [0.0, 0.0, 1.0],
        };
        let mut payload = BitWriter::new();
        line.encode_payload(&mut payload).unwrap();
        ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef {
                code: 0,
                value: 0x10,
            },
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::new(), // empty payload for R2000 round-trip
            handles: ObjectHandles {
                owner: Some(HandleRef {
                    code: 5,
                    value: 0x01,
                }),
                reactors: Vec::new(),
                x_dictionary: None,
                layer: HandleRef {
                    code: 5,
                    value: 0x02,
                },
                linetype: None,
                plot_style: None,
                material: None,
            },
        }
    }

    #[test]
    fn empty_r2000_file_round_trips() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let bytes = assemble_r2000(parts).unwrap();
        let file = parse_r2000(&bytes).unwrap();
        assert_eq!(file.version, Version::R2000);
        assert_eq!(file.classes.classes.len(), 0);
        assert_eq!(file.objects.len(), 0);
    }

    #[test]
    fn single_line_r2000_file_round_trips() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: vec![one_line_record()],
        };
        let bytes = assemble_r2000(parts).unwrap();
        let file = parse_r2000(&bytes).unwrap();
        assert_eq!(file.objects.len(), 1);
        let object = &file.objects[0];
        assert_eq!(object.map_handle, 0x10);
        assert_eq!(object.object_type, ObjectType::Line);
        assert_eq!(
            object.record_handle,
            HandleRef {
                code: 0,
                value: 0x10
            }
        );
        // raw_bytes should round-trip through ObjectRecord::decode_with
        // for the empty-payload one_line_record() fixture.
        let (round, _payload, _) =
            ObjectRecord::decode_with(Version::R2000, &object.raw_bytes, |_ot, _common, _r| Ok(()))
                .unwrap();
        assert_eq!(round.object_type, ObjectType::Line);
        assert_eq!(
            round.handle,
            HandleRef {
                code: 0,
                value: 0x10
            }
        );
    }

    #[test]
    fn r2000_file_rejects_truncated_header() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let bytes = assemble_r2000(parts).unwrap();
        // Truncate right after the signature so the file-header parse
        // fails before any section walking.
        let result = parse_r2000(&bytes[..8]);
        assert!(
            result.is_err(),
            "expected truncation error, got {:?}",
            result
        );
    }

    #[test]
    fn r2000_file_emits_header_end_sentinel_after_locator_crc() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let bytes = assemble_r2000(parts).unwrap();
        // HEADER_END must appear at byte FIXED_HEADER_LEN + 9*3 + 2.
        let expected_offset = FIXED_HEADER_LEN + LOCATOR_COUNT * 9 + 2;
        assert_eq!(
            &bytes[expected_offset..expected_offset + HEADER_END.len()],
            &HEADER_END,
            "HEADER_END sentinel missing at expected offset 0x{:x}",
            expected_offset
        );
    }

    #[test]
    fn r2000_file_writes_three_locators_with_canonical_ids() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let bytes = assemble_r2000(parts).unwrap();
        let count = u32::from_le_bytes([bytes[0x15], bytes[0x16], bytes[0x17], bytes[0x18]]);
        assert_eq!(count, 3, "section locator count must be exactly 3");
        // Locator ids: byte FIXED_HEADER_LEN + 9*i.
        assert_eq!(bytes[FIXED_HEADER_LEN], 0, "locator[0].id must be HEADER");
        assert_eq!(
            bytes[FIXED_HEADER_LEN + 9],
            1,
            "locator[1].id must be CLASSES"
        );
        assert_eq!(
            bytes[FIXED_HEADER_LEN + 18],
            2,
            "locator[2].id must be HANDLES"
        );
    }
}
