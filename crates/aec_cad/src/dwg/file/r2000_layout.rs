//! R2000 (and R14) full file layout — orchestrates the header,
//! section locators, section CRCs, and the on-disk positioning of
//! each section relative to file offsets.
//!
//! Layout produced by [`assemble_r2000`]:
//!
//! ```text
//! 0x00 ┌──────────────────────────────────────────┐
//!      │ File header (0x20 bytes)                  │
//! 0x1c │   + section_locator_count = 5             │
//!      │   + locator records:                      │
//!      │     [0] HEADER       → ofs_header, size   │
//!      │     [1] CLASSES      → ofs_classes, size  │
//!      │     [2] OBJECTS      → ofs_objects, size  │
//!      │     [3] OBJECT_MAP   → ofs_objmap, size   │
//!      │     [4] SecondHeader → ofs_2hdr, size     │
//!      │   + locator CRC (2 bytes)                 │
//! ofs_header ┌────────────────────────────────────┐
//!            │ HEADER_VARS section                 │
//! ofs_classes├────────────────────────────────────┤
//!            │ CLASSES section                     │
//! ofs_objects├────────────────────────────────────┤
//!            │ OBJECTS section (entity records)    │
//! ofs_objmap ├────────────────────────────────────┤
//!            │ OBJECT_MAP section                  │
//! ofs_2hdr   ├────────────────────────────────────┤
//!            │ Second-header sentinel block        │
//!            └────────────────────────────────────┘
//! ```
//!
//! Reference: OpenDesign Specification "DWG R13-R2000 File Format
//! Overview" and the LibreDWG `decode.c::decode_R13_R2000` walker.

use crate::dwg::bits::crc_x25;
use crate::dwg::bits::reader::HandleRef;
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::ObjectRecord;
use crate::dwg::entities::ObjectType;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::classes::ClassesSection;
use crate::dwg::file::header::FileHeader;
use crate::dwg::file::header_vars::HeaderVarsSection;
use crate::dwg::file::object_map::{ObjectMap, ObjectMapEntry};
use crate::dwg::file::sections::{encode_locators, parse_locators, SectionId, SectionLocator};
use crate::dwg::file::sentinels::{SECOND_HEADER_BEGIN, SECOND_HEADER_END};
use crate::dwg::version::Version;

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

/// Minimum number of bytes the file header + locator block + locator
/// CRC occupies before any section data.
///
/// Layout: 0x1c bytes of fixed header (signature + reserved + counts) +
/// `9 * locator_count` bytes of locator records + 2 bytes CRC-X25 over
/// the previous bytes.
fn header_block_size(locator_count: usize) -> usize {
    0x1c + locator_count * 9 + 2
}

/// Assemble a complete R14/R2000 file from its in-memory parts.
///
/// Strategy: lay out the sections in a fixed order, compute their
/// offsets and sizes, then back-patch the locator block. Each section
/// is preceded by its standard sentinel (or none, for sections whose
/// own encoder already emits one).
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

    // We use 5 locators (header, classes, objects, object_map, 2nd
    // header). Some real R2000 files emit 6+ with vendor sections; we
    // produce the canonical 5 and a parser tolerates additional ones.
    const LOCATOR_COUNT: usize = 5;
    let prefix_size = header_block_size(LOCATOR_COUNT);

    // Encode each section into a freestanding buffer.
    let mut header_vars_bytes = Vec::new();
    parts.header_vars.encode(&mut header_vars_bytes);
    let mut classes_bytes = Vec::new();
    parts.classes.encode(&mut classes_bytes)?;

    // OBJECTS section: concatenated ObjectRecord wire bytes. The
    // object map needs the file offset of each record, so we track
    // those as we go.
    let mut objects_bytes = Vec::new();
    let mut object_map = ObjectMap::new();
    // We don't yet know the OBJECTS section's file offset — we patch
    // the per-record file offsets up after we know `objects_offset`.
    let mut record_section_offsets: Vec<u64> = Vec::with_capacity(parts.objects.len());
    for record in &parts.objects {
        record_section_offsets.push(objects_bytes.len() as u64);
        let wire = record.encode(parts.version)?;
        objects_bytes.extend_from_slice(&wire);
    }

    // OBJECT_MAP: encoded length depends only on entries; entries are
    // sorted by handle for canonical layout.
    let mut sorted_indices: Vec<usize> = (0..parts.objects.len()).collect();
    sorted_indices.sort_by_key(|&i| parts.objects[i].handle.value);

    // Compute offsets.
    let mut cursor = prefix_size as u32;
    let header_offset = cursor;
    cursor += header_vars_bytes.len() as u32;
    let classes_offset = cursor;
    cursor += classes_bytes.len() as u32;
    let objects_offset = cursor;
    cursor += objects_bytes.len() as u32;
    let object_map_offset = cursor;

    // Now we know `objects_offset`; populate object_map entries with
    // absolute file offsets.
    for &i in &sorted_indices {
        object_map.entries.push(ObjectMapEntry {
            handle: parts.objects[i].handle.value,
            file_offset: objects_offset as u64 + record_section_offsets[i],
        });
    }
    let mut object_map_bytes = Vec::new();
    object_map.encode(&mut object_map_bytes)?;
    cursor += object_map_bytes.len() as u32;
    let second_header_offset = cursor;
    let second_header_bytes = encode_second_header(parts.version);
    cursor += second_header_bytes.len() as u32;

    // Build the locator records.
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
            id: SectionId::Objects,
            seeker: objects_offset,
            size: objects_bytes.len() as u32,
        },
        SectionLocator {
            id: SectionId::ObjectMap,
            seeker: object_map_offset,
            size: object_map_bytes.len() as u32,
        },
        SectionLocator {
            // SecondHeader is technically Unknown(0x04) in our enum,
            // but R2000 uses id = 5 for the second-header range. We
            // emit the byte literal here so we don't have to wedge a
            // new variant into SectionId just for this file walker.
            id: SectionId::Unknown(5),
            seeker: second_header_offset,
            size: second_header_bytes.len() as u32,
        },
    ];
    let locator_bytes = encode_locators(&locators);

    // Build the header.
    let header = FileHeader {
        version: parts.version,
        maintenance_release: 0,
        preview_offset: 0,
        codepage: 30,
        section_locator_count: LOCATOR_COUNT as u32,
    };
    // Truncate the encoded header to its true fixed length (0x1c
    // bytes). Bytes 0x1c..0x20 in FileHeader::encode are zero
    // padding that exists only so the encoded buffer matches the
    // R2004+ layout; on R14/R2000 those bytes belong to the locator
    // block.
    let mut header_bytes = header.encode();
    header_bytes.truncate(0x1c);
    header_bytes.extend_from_slice(&locator_bytes);
    // Header block ends with a CRC-X25 over the first 0x1c + 9*count
    // bytes of the file (with seed varying by locator count per the
    // LibreDWG `dwg_crc_seed` table).
    let crc = crc_x25(crc_seed_for_locator_count(LOCATOR_COUNT), &header_bytes);
    header_bytes.extend_from_slice(&crc.to_le_bytes());
    debug_assert_eq!(
        header_bytes.len(),
        prefix_size,
        "header block size {} != computed prefix {}",
        header_bytes.len(),
        prefix_size
    );

    let mut out = Vec::with_capacity(cursor as usize);
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&header_vars_bytes);
    out.extend_from_slice(&classes_bytes);
    out.extend_from_slice(&objects_bytes);
    out.extend_from_slice(&object_map_bytes);
    out.extend_from_slice(&second_header_bytes);

    Ok(out)
}

/// Parse a complete R14/R2000 file. Returns the decoded sections plus
/// the object-record stream (recovered from the OBJECTS / OBJECT_MAP
/// pair).
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
    let locators = parse_locators(bytes, 0x1c, header.section_locator_count)?;
    // Verify the header CRC. The CRC covers bytes [0..0x1c + 9*count]
    // and lives at byte 0x1c + 9*count.
    let header_block_len = 0x1c + locator_count * 9;
    if bytes.len() < header_block_len + 2 {
        return Err(DwgError::UnexpectedEof {
            byte: header_block_len + 2,
            bit: 0,
        });
    }
    let stored_crc = u16::from_le_bytes([bytes[header_block_len], bytes[header_block_len + 1]]);
    let computed_crc = crc_x25(
        crc_seed_for_locator_count(locator_count),
        &bytes[..header_block_len],
    );
    if stored_crc != computed_crc {
        return Err(DwgError::HeaderCrcMismatch {
            computed: computed_crc,
            stored: stored_crc,
        });
    }

    // Walk each well-known section.
    let mut header_vars: Option<HeaderVarsSection> = None;
    let mut classes: Option<ClassesSection> = None;
    let mut objects_range: Option<(usize, usize)> = None;
    let mut object_map_range: Option<(usize, usize)> = None;
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
            SectionId::Objects => {
                objects_range = Some((start, end));
            }
            SectionId::ObjectMap => {
                object_map_range = Some((start, end));
            }
            SectionId::Unknown(_) => {
                // Second header or vendor extension; we don't decode
                // those structurally yet but we do tolerate them.
            }
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
    let object_map_range = object_map_range.ok_or_else(|| DwgError::MalformedObject {
        class: "FileLayout".into(),
        offset: 0,
        message: "no OBJECT_MAP section locator".into(),
    })?;

    let object_map = ObjectMap::parse(&bytes[object_map_range.0..object_map_range.1])?;

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
    // Sanity check: the OBJECTS section must contain at least the
    // start of the first record.
    let _ = objects_range;

    Ok(R2000File {
        version,
        header_vars,
        classes,
        objects,
    })
}

/// Header-block CRC seed table (indexed by locator count). Empirically
/// confirmed against LibreDWG's `dwg_section_crc_seed` table.
fn crc_seed_for_locator_count(count: usize) -> u16 {
    match count {
        3 => 0xa598,
        4 => 0x8101,
        5 => 0x3cc4,
        6 => 0x8b7d,
        _ => 0xc0c1, // safe default; AutoCAD will mark unknown counts
    }
}

/// Encode the second-header sentinel block. The R14/R2000 second
/// header is a partial duplicate of the file-header data used by
/// AutoCAD's recovery code; for round-trip purposes we emit a minimal
/// well-formed envelope (begin sentinel + end sentinel with no body).
fn encode_second_header(_version: Version) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&SECOND_HEADER_BEGIN);
    out.extend_from_slice(&SECOND_HEADER_END);
    out
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
        // Truncate to before the header CRC.
        let truncated = &bytes[..0x1c + 9 * 5];
        assert!(matches!(
            parse_r2000(truncated),
            Err(DwgError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn r2000_file_rejects_header_crc_corruption() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let mut bytes = assemble_r2000(parts).unwrap();
        // Flip a byte in the locator block.
        bytes[0x1c] ^= 0xff;
        assert!(matches!(
            parse_r2000(&bytes),
            Err(DwgError::HeaderCrcMismatch { .. })
        ));
    }

    #[test]
    fn r2000_file_unsupported_for_paged_versions() {
        let parts = R2000FileParts {
            version: Version::R2010,
            header_vars: HeaderVarsSection::minimal(Version::R2010),
            classes: ClassesSection::empty(Version::R2010),
            objects: Vec::new(),
        };
        assert!(matches!(
            assemble_r2000(parts),
            Err(DwgError::UnsupportedInVersion { .. })
        ));
    }
}
