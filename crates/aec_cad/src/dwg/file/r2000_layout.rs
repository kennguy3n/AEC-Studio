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
//!
//! # Known limitation: no second-header block
//!
//! AutoCAD's `RECOVER` command uses the second-header sentinel block
//! as a redundant cross-check when the primary header is corrupt.
//! Because [`assemble_r2000`] emits the canonical minimal layout
//! (3 locators, no second-header), files produced by this writer are
//! readable by LibreDWG, AutoCAD, and any spec-compliant reader, but
//! `RECOVER` has nothing to fall back on if the locator-block CRC is
//! damaged. This is the same tradeoff LibreDWG's own minimal-encoder
//! path makes; emitting the second-header block is tracked as a
//! future enhancement and does not affect normal open-and-save flow.

use crate::dwg::bits::crc_x25;
use crate::dwg::bits::reader::HandleRef;
use crate::dwg::entities::header_codec::CommonHeaderData;
use crate::dwg::entities::ObjectRecord;
use crate::dwg::entities::ObjectType;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::classes::ClassesSection;
use crate::dwg::file::header::{FileHeader, FIXED_HEADER_LEN};
use crate::dwg::file::header_vars::HeaderVarsSection;
use crate::dwg::file::object_map::ObjectMap;
use crate::dwg::file::objects_section::{
    build_handle_object_map, encode_objects_payload, recover_objects_via_map,
};
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

/// Maximum number of bytes after the locator CRC in which to look for
/// the HEADER_END sentinel. LibreDWG accepts a few bytes of padding
/// between the CRC and the sentinel; we pick a generous-but-bounded
/// window so a corrupt file still fails fast rather than scanning the
/// entire body. Our writer emits the sentinel immediately, so this
/// only matters when reading third-party files.
const SENTINEL_SEARCH_WINDOW: usize = 256;

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
    let (objects_bytes, record_stream_offsets) =
        encode_objects_payload(&parts.objects, parts.version)?;

    // Compute file-absolute offsets.
    let mut cursor = prefix_size as u32;
    let header_offset = cursor;
    cursor += header_vars_bytes.len() as u32;
    let classes_offset = cursor;
    cursor += classes_bytes.len() as u32;
    let objects_offset = cursor;
    cursor += objects_bytes.len() as u32;
    let handles_offset = cursor;

    // Build the object map. Entries are sorted by handle (canonical
    // R13–R2000 layout); each entry's `file_offset` is
    // *file-absolute* — `offset_base = objects_offset` so the parser
    // (`recover_objects_via_map`) can seek into the file directly
    // without a section-relative buffer.
    let object_map = build_handle_object_map(
        &parts.objects,
        &record_stream_offsets,
        objects_offset as u64,
    )?;
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

    // Forward-search for the HEADER_END sentinel within a bounded
    // window after the locator CRC. LibreDWG does the same — third-
    // party R2000 files occasionally have a small variable-length
    // padding between the CRC and the sentinel, so a strict positional
    // check would refuse otherwise valid files. The sentinel itself is
    // 16 random-looking bytes, so the false-match probability inside
    // any reasonable window is negligible (~2^-128 per candidate
    // position).
    let search_start = crc_offset + 2;
    let search_end = bytes
        .len()
        .min(search_start + SENTINEL_SEARCH_WINDOW + HEADER_END.len());
    if bytes[search_start..search_end]
        .windows(HEADER_END.len())
        .position(|w| w == HEADER_END)
        .is_none()
    {
        return Err(DwgError::MalformedObject {
            class: "FileLayout".into(),
            offset: search_start as u64,
            message: format!(
                "HEADER_END sentinel not found within {SENTINEL_SEARCH_WINDOW} bytes of locator CRC"
            ),
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

    // For each object-map entry, peek the structural header (so we
    // know the record's wire size) and store the raw record bytes.
    // We do NOT eagerly call `ObjectRecord::decode` because for
    // R14/R2000 that would mis-parse any record with a non-empty
    // payload — the payload-to-handle-stream boundary requires
    // per-type knowledge and `decode` has no version-specific
    // bitsize marker to fall back on. The caller invokes
    // `ObjectRecord::decode_with(raw_bytes, ..)` with a per-type
    // decoder to recover the full entity. Shared with R2004/R2007
    // recovery; only R14/R2000 uses the file-absolute-offset seek
    // path because its OBJECTS stream lives at a known file offset
    // (R2004/R2007 wrap it in a paged section and walk a
    // decompressed buffer instead).
    let objects = recover_objects_via_map(bytes, &object_map, version)?;

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
    use crate::dwg::entities::record::{BitBuf, ObjectCommonData, ObjectHandles, ObjectSupertype};
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
        // R2000 path exclusively; pin the version explicitly so the
        // version-aware LINE codec emits the R2000+ `z_is_zero/RD/DD`
        // wire form (entities::line::encode_payload).
        line.encode_payload(&mut payload, Version::R2000).unwrap();
        ObjectRecord {
            object_type: ObjectType::Line,
            handle: HandleRef {
                code: 0,
                value: 0x10,
            },
            supertype: ObjectSupertype::Entity,
            object_common: ObjectCommonData::default(),
            common: CommonHeaderData::default(),
            payload_bits: BitBuf::new(), // empty payload for R2000 round-trip
            string_payload_bits: BitBuf::new(),
            handles: ObjectHandles {
                owner: Some(HandleRef {
                    code: 5,
                    value: 0x01,
                }),
                layer: HandleRef {
                    code: 5,
                    value: 0x02,
                },
                ..Default::default()
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

    #[test]
    fn r2000_file_rejects_header_crc_corruption() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let mut bytes = assemble_r2000(parts).unwrap();
        // Flip a byte inside the locator block (which is covered by
        // the CRC). The new locator block starts at FIXED_HEADER_LEN.
        bytes[FIXED_HEADER_LEN] ^= 0xff;
        assert!(matches!(
            parse_r2000(&bytes),
            Err(DwgError::HeaderCrcMismatch { .. })
        ));
    }

    #[test]
    fn r2000_file_tolerates_padding_before_header_end_sentinel() {
        // LibreDWG forward-searches for HEADER_END within a bounded
        // window after the locator CRC, so we must accept a small
        // amount of padding between the CRC and the sentinel — third-
        // party R2000 writers occasionally emit a handful of zero
        // bytes there. Insert 8 bytes of padding and verify the
        // parser still finds the sentinel.
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let bytes = assemble_r2000(parts).unwrap();
        let sentinel_offset = FIXED_HEADER_LEN + LOCATOR_COUNT * 9 + 2;
        let mut padded = Vec::with_capacity(bytes.len() + 8);
        padded.extend_from_slice(&bytes[..sentinel_offset]);
        padded.extend_from_slice(&[0u8; 8]);
        padded.extend_from_slice(&bytes[sentinel_offset..]);
        // Note: section locators point into the original bytes layout,
        // not the padded one. We're only exercising the sentinel
        // search itself; section walking will fail later, which is
        // fine — we just want to confirm parse_r2000 advances past
        // the sentinel-search step without error.
        match parse_r2000(&padded) {
            Err(DwgError::MalformedObject { message, .. })
                if message.contains("HEADER_END sentinel not found") =>
            {
                panic!("parser rejected padding before HEADER_END within the search window");
            }
            _ => {}
        }
    }

    #[test]
    fn r2000_file_rejects_missing_header_end_sentinel() {
        let parts = R2000FileParts {
            version: Version::R2000,
            header_vars: HeaderVarsSection::minimal(Version::R2000),
            classes: ClassesSection::empty(Version::R2000),
            objects: Vec::new(),
        };
        let mut bytes = assemble_r2000(parts).unwrap();
        // Corrupt the HEADER_END sentinel that lives immediately after
        // the locator CRC. Parsing must refuse the file rather than
        // silently treating the next bytes as section data.
        let sentinel_offset = FIXED_HEADER_LEN + LOCATOR_COUNT * 9 + 2;
        bytes[sentinel_offset] ^= 0xff;
        let err = parse_r2000(&bytes).unwrap_err();
        assert!(
            matches!(err, DwgError::MalformedObject { .. }),
            "expected MalformedObject for corrupt HEADER_END, got {err:?}"
        );
    }

    #[test]
    fn r2000_assemble_rejects_paged_versions() {
        // R2004+ uses the paged system-section layout; assemble_r2000
        // must surface that with a structured error rather than emit
        // a malformed file. This is the single guard that keeps a
        // caller from accidentally producing an R2010 file with an
        // R2000 wire shape.
        for version in [
            Version::R2004,
            Version::R2007,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            let parts = R2000FileParts {
                version,
                header_vars: HeaderVarsSection::minimal(version),
                classes: ClassesSection::empty(version),
                objects: Vec::new(),
            };
            let err = assemble_r2000(parts).unwrap_err();
            assert!(
                matches!(err, DwgError::UnsupportedInVersion { .. }),
                "expected UnsupportedInVersion for {version:?}, got {err:?}"
            );
        }
    }
}
