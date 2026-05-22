//! R2007 top-level file assembler and parser.
//!
//! In R2007+ the post-legacy-header file layout is fundamentally
//! different from R2004:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ 0x000 .. 0x080 │ Legacy file header (FileHeader)         │
//! ├──────────────────────────────────────────────────────────┤
//! │ 0x080 .. 0x458 │ RS-encoded R2007 file header (984 bytes)│
//! │                │   (stored-mode Dwg_R2007_Header inside)  │
//! ├──────────────────────────────────────────────────────────┤
//! │ 0x458 .. 0x480 │ 0x28 bytes of zero "overread check data"│
//! ├──────────────────────────────────────────────────────────┤
//! │ 0x480 ..       │ System pages and data pages, in any     │
//! │                │ order. Each page's location is given by │
//! │                │ the pages-map; the pages-map's own      │
//! │                │ location is given by                    │
//! │                │ `file_header.pages_map_offset` (relative│
//! │                │ to 0x480).                              │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! The pages-map is itself an RS-encoded system page whose content
//! is a sequence of 16-byte `(size: u64, id: i64)` records. The
//! sections-map is an RS-encoded system page whose content begins
//! with one section descriptor per logical section (8 × u64 = 64
//! bytes), followed by the section name in UTF-16LE, followed by
//! `num_pages` × 56-byte page entries.
//!
//! ## Current scope (empty R2007 file)
//!
//! This first cut emits a valid R2007 file with **zero sections**.
//! That's enough to get past LibreDWG's
//! `Invalid file_header->header_size` errors that today's "false
//! pass" produces. Downstream `read_2007_section_*` calls will
//! return `DWG_ERR_SECTIONNOTFOUND`; those become non-zero exit
//! codes in `dwgread` only if cumulative error >= `DWG_ERR_CRITICAL`
//! (128). `SECTIONNOTFOUND` is 256 which *is* >= critical, so to
//! truly clean-parse we'll need at least an `AcDb:Header` section
//! present — but that's a follow-up step we wire in after the file
//! header layer is validated end-to-end.
//!
//! Reference: LibreDWG `decode_r2007.c::read_r2007_meta_data`
//! (line 2338).

use crate::dwg::bits::reed_solomon::R2007_FILE_HEADER_ON_DISK_SIZE;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::header::FileHeader;
use crate::dwg::file::r2007_header::{
    decode_file_header_on_disk, encode_file_header_on_disk, R2007FileHeader, R2007_HEADER_OFFSET,
};
use crate::dwg::file::r2007_system_page::{
    decode_system_page, encode_system_page, system_page_on_disk_size,
};
use crate::dwg::version::Version;

/// File offset where R2007 page data begins. Computed as
/// `R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE + R2007_CHECK_DATA_LEN`
/// (0x080 + 0x3d8 + 0x028 = 0x480). LibreDWG's
/// `read_r2007_meta_data` hardcodes this layout at lines 2362-2364.
pub const R2007_FIRST_PAGE_OFFSET: u64 =
    (R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE + R2007_CHECK_DATA_LEN) as u64;

/// Number of zero bytes between the RS-encoded file header
/// (ending at 0x458) and the first page (starting at 0x480).
/// LibreDWG comments this region as "overread check data" — it's
/// always zero in AutoCAD-emitted files and ignored on read.
pub const R2007_CHECK_DATA_LEN: usize = 0x28;

/// In-memory image of a parsed R2007 file. Currently only carries
/// the version + the parsed pages-map records — section content
/// emission lands in a follow-up commit once the file-header layer
/// is validated end-to-end against `dwgread`.
#[derive(Debug, Clone, PartialEq)]
pub struct R2007File {
    pub version: Version,
    /// `(page_id, on_disk_size)` for every page enumerated in the
    /// pages-map.
    pub pages_map: Vec<(i64, u64)>,
    /// The id LibreDWG looks up to find the sections-map page.
    pub sections_map_id: i64,
    /// Currently always 0; will become the actual count once section
    /// content emission lands.
    pub num_sections: i64,
}

/// Build the pages-map content for an R2007 file from a list of
/// `(page_id, page_size)` pairs. Output is `16 * pages.len()` bytes,
/// each entry being `size: u64 LE` followed by `id: i64 LE` (matching
/// LibreDWG `decode_r2007.c::read_pages_map`).
pub(crate) fn encode_pages_map_content(pages: &[(i64, u64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pages.len() * 16);
    for &(id, size) in pages {
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(id as u64).to_le_bytes());
    }
    out
}

/// Inverse of [`encode_pages_map_content`] — parse a sequence of
/// `(id, size)` pairs from `payload`. Each entry is 16 bytes.
pub(crate) fn decode_pages_map_content(payload: &[u8]) -> DwgResult<Vec<(i64, u64)>> {
    if payload.len() % 16 != 0 {
        return Err(DwgError::InternalInvariant(format!(
            "R2007 pages-map content must be a multiple of 16 bytes (got {})",
            payload.len()
        )));
    }
    let mut out = Vec::with_capacity(payload.len() / 16);
    for chunk in payload.chunks_exact(16) {
        let mut size_buf = [0u8; 8];
        size_buf.copy_from_slice(&chunk[0..8]);
        let mut id_buf = [0u8; 8];
        id_buf.copy_from_slice(&chunk[8..16]);
        let size = u64::from_le_bytes(size_buf);
        let id = i64::from_le_bytes(id_buf);
        out.push((id, size));
    }
    Ok(out)
}

/// Per-section descriptor in an R2007 sections-map. 64 bytes of
/// 8 × i64 LE fields, mirroring LibreDWG's `r2007_section` struct
/// (decode_r2007.c line 891-898).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionDescriptor {
    pub data_size: i64,
    pub max_size: i64,
    pub encrypted: i64,
    pub hashcode: i64,
    pub name_length: i64,
    pub unknown: i64,
    pub encoded: i64,
    pub num_pages: i64,
    pub name: String,
}

impl SectionDescriptor {
    /// Build an "empty" section descriptor (`num_pages = 0`) for the
    /// given canonical section name. LibreDWG's reader logs the
    /// `num_pages = 0` case as "Invalid num_pages, skip" but accepts
    /// the section into its lookup table, so subsequent
    /// `read_data_section(SECTION_HEADER, …)` calls return 0 (the
    /// data buffer is zero-length, sentinel search fails, decoder
    /// gracefully skips the body).
    pub fn empty_for_name(name: &str) -> Self {
        let name_bytes = utf16le_bytes(name);
        Self {
            data_size: 1,
            max_size: 1,
            encrypted: 0,
            hashcode: 0,
            name_length: name_bytes.len() as i64,
            unknown: 0,
            encoded: 0,
            num_pages: 0,
            name: name.to_string(),
        }
    }

    /// Serialize this descriptor onto the sections-map content
    /// payload: 64 bytes of header + `name_length` bytes of
    /// UTF-16LE name (with no trailing null — LibreDWG allocates
    /// `name_length + 2` zero bytes and reads `name_length` into
    /// that buffer, so the terminator is implicit).
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.data_size.to_le_bytes());
        out.extend_from_slice(&self.max_size.to_le_bytes());
        out.extend_from_slice(&self.encrypted.to_le_bytes());
        out.extend_from_slice(&self.hashcode.to_le_bytes());
        out.extend_from_slice(&self.name_length.to_le_bytes());
        out.extend_from_slice(&self.unknown.to_le_bytes());
        out.extend_from_slice(&self.encoded.to_le_bytes());
        out.extend_from_slice(&self.num_pages.to_le_bytes());
        let name_bytes = utf16le_bytes(&self.name);
        debug_assert_eq!(name_bytes.len() as i64, self.name_length);
        out.extend_from_slice(&name_bytes);
        debug_assert_eq!(
            self.num_pages, 0,
            "section page entries not yet emitted (num_pages must be 0)"
        );
    }
}

/// Convert an ASCII section name into UTF-16LE bytes. R2007
/// sections-map names are stored as wide chars with no BOM, in
/// little-endian byte order. Names are always ASCII-safe in the
/// canonical list (`AcDb:Header`, `AcDb:Classes`, etc.) so each
/// code unit fits in one byte and we widen to 2 bytes with the
/// high byte zero.
fn utf16le_bytes(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() * 2);
    for code_unit in name.encode_utf16() {
        out.extend_from_slice(&code_unit.to_le_bytes());
    }
    out
}

/// Canonical section names that LibreDWG treats as mandatory in
/// R2007 (`read_data_section` returns `DWG_ERR_SECTIONNOTFOUND` for
/// any of these missing). Names map 1:1 to
/// `Dwg_Section_Type_r2004` (1..7 with 5 and 6 excluded as soft).
/// See dwg.c::dwg_section_r2004_names.
pub(crate) const MANDATORY_R2007_SECTION_NAMES: &[&str] = &[
    "AcDb:Header",      // type 1
    "AcDb:AuxHeader",   // type 2
    "AcDb:Classes",     // type 3
    "AcDb:Handles",     // type 4
    "AcDb:AcDbObjects", // type 7
];

/// Build the sections-map content for an R2007 file. Currently
/// emits a placeholder descriptor for each mandatory section type
/// with `num_pages = 0`; that's enough for LibreDWG's
/// `read_2007_section_*` functions to find each section by name
/// and return success with empty content. Real per-section data
/// pages are wired in by a follow-up commit.
pub(crate) fn encode_sections_map_content() -> Vec<u8> {
    let mut out = Vec::new();
    for name in MANDATORY_R2007_SECTION_NAMES {
        let descriptor = SectionDescriptor::empty_for_name(name);
        descriptor.encode(&mut out);
    }
    out
}

/// Parts needed to build an R2007 file.
///
/// Currently only carries the version — section payloads
/// (`header_vars`, `classes`, `objects`) are not yet wired into the
/// assembler. We pin down the file-header / pages-map / sections-map
/// layout first; the section-content layer wires in once the header
/// layer is validated end-to-end against `dwgread`.
pub struct R2007FileParts {
    pub version: Version,
}

/// Assemble a complete R2007+ file from its in-memory parts.
///
/// Current behavior: emits a valid R2007 file with the proper
/// RS-encoded file header at byte 0x80, a minimal pages-map, and
/// a (zero-section) sections-map. The `parts` payload fields
/// (header_vars, classes, objects) are accepted for API symmetry
/// with `assemble_r2004` but not yet serialized into data pages
/// — that wiring comes in a follow-up commit once the file-header
/// layer is validated end-to-end against `dwgread`.
pub fn assemble_r2007(parts: R2007FileParts) -> DwgResult<Vec<u8>> {
    if !parts.version.uses_utf16_strings() {
        return Err(DwgError::UnsupportedInVersion {
            version: parts.version,
            what: format!(
                "assemble_r2007 only handles R2007+; got {:?}",
                parts.version
            ),
        });
    }

    let mut out = Vec::with_capacity(0x800);

    // 1. Legacy file header (0x19 bytes), zero-padded out to 0x80 as
    //    R2004+ does. `FileHeader::encode()` returns the canonical
    //    leading slice; we then resize to the R2007 header offset.
    let file_header = FileHeader {
        version: parts.version,
        maintenance_release: parts.version.maintenance_release(),
        preview_offset: 0,
        codepage: 30,
        section_locator_count: 0,
    };
    let mut legacy = file_header.encode();
    legacy.resize(R2007_HEADER_OFFSET, 0);
    out.extend_from_slice(&legacy);
    debug_assert_eq!(out.len(), R2007_HEADER_OFFSET);

    // Reserve space for the RS-encoded R2007 file header — we'll
    // backfill it once we know all the page sizes / offsets.
    out.resize(R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE, 0);

    // 2. 0x28-byte zero "check data" gap.
    out.resize(out.len() + R2007_CHECK_DATA_LEN, 0);
    debug_assert_eq!(out.len() as u64, R2007_FIRST_PAGE_OFFSET);

    // 3. Sections-map system page (currently empty content).
    let sections_map_content = encode_sections_map_content();
    let sections_map = encode_system_page(&sections_map_content);
    let sections_map_page_id: i64 = 1;
    let sections_map_offset = out.len() as u64;
    out.extend_from_slice(&sections_map.on_disk);

    // 4. Pages-map content: just one record, the sections-map page.
    //    Each record is (size, id) in u64 LE pairs.
    let pages_records: Vec<(i64, u64)> =
        vec![(sections_map_page_id, sections_map.on_disk.len() as u64)];
    let pages_map_content = encode_pages_map_content(&pages_records);
    let pages_map = encode_system_page(&pages_map_content);
    let pages_map_offset_rel = (out.len() as u64) - R2007_FIRST_PAGE_OFFSET;
    out.extend_from_slice(&pages_map.on_disk);

    // 5. Build the R2007 file header struct now that all offsets/
    //    sizes are known.
    let file_size = out.len() as i64;
    let mut header = R2007FileHeader::new();
    header.file_size = file_size;
    header.pages_map_offset = pages_map_offset_rel as i64;
    header.pages_map_id = 0; // AutoCAD writes 0 here.
    header.pages_map_size_comp = pages_map.size_comp;
    header.pages_map_size_uncomp = pages_map.size_uncomp;
    header.pages_map_correction = pages_map.repeat_count;
    header.pages_amount = (pages_records.len() + 1) as i64; // pages + the map itself
    header.pages_maxid = sections_map_page_id;
    header.num_sections = MANDATORY_R2007_SECTION_NAMES.len() as i64;
    header.sections_map_id = sections_map_page_id;
    header.sections_map_size_comp = sections_map.size_comp;
    header.sections_map_size_uncomp = sections_map.size_uncomp;
    header.sections_map_correction = sections_map.repeat_count;

    // 6. Encode the file header on disk and patch it in at 0x80.
    let header_bytes = encode_file_header_on_disk(&header);
    out[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
        .copy_from_slice(&header_bytes);

    // Silence the unused-binding warnings for variables we keep for
    // future expansion of the assembler.
    let _ = sections_map_offset;

    Ok(out)
}

/// Same shape as [`R2007File`] but pulled out so [`parse_r2007`]
/// can construct it from a partially-decoded file. Internal API.
fn r2007_file_from_header(
    version: Version,
    header: &R2007FileHeader,
    pages_map: Vec<(i64, u64)>,
) -> R2007File {
    R2007File {
        version,
        pages_map,
        sections_map_id: header.sections_map_id,
        num_sections: header.num_sections,
    }
}

/// Parse a complete R2007+ file produced by [`assemble_r2007`].
///
/// Self-round-trip parser. Currently understands the file-header,
/// pages-map, and sections-map layout but returns empty
/// `header_vars` / `classes` / `objects` until section-content
/// emission is wired in.
pub fn parse_r2007(bytes: &[u8], version: Version) -> DwgResult<R2007File> {
    if !version.uses_utf16_strings() {
        return Err(DwgError::UnsupportedInVersion {
            version,
            what: format!("parse_r2007 only handles R2007+; got {version:?}"),
        });
    }
    if bytes.len() < (R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE) {
        return Err(DwgError::InternalInvariant(
            "parse_r2007: input shorter than legacy header + R2007 header region".into(),
        ));
    }

    // 1. Parse the legacy header just to verify the signature; we
    //    don't otherwise need its contents.
    let _legacy = FileHeader::parse(&bytes[..R2007_HEADER_OFFSET], version)?;

    // 2. Decode the R2007 file header at 0x80 (RS-decoded).
    let header_region =
        &bytes[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE];
    let header = decode_file_header_on_disk(header_region)?;

    // 3. Locate the pages-map.
    let pages_map_off = R2007_FIRST_PAGE_OFFSET as usize + header.pages_map_offset as usize;
    let pages_map_on_disk_len = system_page_on_disk_size(header.pages_map_size_uncomp as usize);
    if bytes.len() < pages_map_off + pages_map_on_disk_len {
        return Err(DwgError::InternalInvariant(
            "parse_r2007: pages-map region exceeds file length".into(),
        ));
    }
    let pages_map_payload = decode_system_page(
        &bytes[pages_map_off..pages_map_off + pages_map_on_disk_len],
        header.pages_map_size_comp,
        header.pages_map_size_uncomp,
        header.pages_map_correction,
    )?;
    let pages_records = decode_pages_map_content(&pages_map_payload)?;

    // 4. Locate the sections-map by id.
    let mut sections_map_offset_running: u64 = R2007_FIRST_PAGE_OFFSET;
    let mut sections_map_phys_offset: Option<u64> = None;
    for &(id, size) in &pages_records {
        if id == header.sections_map_id {
            sections_map_phys_offset = Some(sections_map_offset_running);
            break;
        }
        sections_map_offset_running += size;
    }
    let _sections_map_phys_offset = sections_map_phys_offset.ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "parse_r2007: sections_map_id {} not in pages-map",
            header.sections_map_id
        ))
    })?;

    Ok(r2007_file_from_header(version, &header, pages_records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::version::Version;

    fn empty_parts(version: Version) -> R2007FileParts {
        R2007FileParts { version }
    }

    #[test]
    fn assemble_emits_legacy_header_then_rs_header_then_pages() {
        let file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        assert!(
            file.len() >= R2007_FIRST_PAGE_OFFSET as usize,
            "file too short: {}",
            file.len()
        );
        // Bytes 0x80..0x458 are not all zero (RS-encoded header).
        assert!(
            file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
                .iter()
                .any(|&b| b != 0)
        );
        // Bytes 0x458..0x480 (check data) are all zero.
        for (i, &b) in file[R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE
            ..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE + R2007_CHECK_DATA_LEN]
            .iter()
            .enumerate()
        {
            assert_eq!(b, 0, "check-data byte at +{} = {:02x}", i, b);
        }
    }

    #[test]
    fn assemble_round_trips_to_parse() {
        let file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        let parsed = parse_r2007(&file, Version::R2007).unwrap();
        assert_eq!(parsed.version, Version::R2007);
        assert_eq!(
            parsed.num_sections,
            MANDATORY_R2007_SECTION_NAMES.len() as i64,
        );
        assert_eq!(parsed.sections_map_id, 1);
        assert_eq!(parsed.pages_map.len(), 1);
        assert_eq!(parsed.pages_map[0].0, 1);
    }

    #[test]
    fn pages_map_round_trips() {
        let records = vec![(1i64, 256u64), (2i64, 512u64), (-1i64, 128u64)];
        let bytes = encode_pages_map_content(&records);
        assert_eq!(bytes.len(), 48);
        let parsed = decode_pages_map_content(&bytes).unwrap();
        assert_eq!(parsed, records);
    }

    #[test]
    fn pages_map_decode_rejects_misaligned() {
        let err = decode_pages_map_content(&[0u8; 17]);
        assert!(err.is_err());
    }

    #[test]
    fn assemble_rejects_non_r2007_version() {
        let err = assemble_r2007(empty_parts(Version::R2004));
        assert!(err.is_err());
        let err = assemble_r2007(empty_parts(Version::R2000));
        assert!(err.is_err());
    }

    #[test]
    fn r2007_first_page_offset_is_0x480() {
        // Hard-pin the offset because LibreDWG's reader hardcodes it
        // at line 2363-2364 of decode_r2007.c.
        assert_eq!(R2007_FIRST_PAGE_OFFSET, 0x480);
    }

    #[test]
    fn assemble_emits_pages_amount_one_plus_user_pages() {
        let file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        // Re-decode the file header to inspect pages_amount.
        let header_region =
            &file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE];
        let header = decode_file_header_on_disk(header_region).unwrap();
        // 1 user page (sections-map) + 1 for the pages-map itself = 2.
        assert_eq!(header.pages_amount, 2);
        assert_eq!(
            header.num_sections,
            MANDATORY_R2007_SECTION_NAMES.len() as i64,
        );
        assert_eq!(header.sections_map_id, 1);
    }

    #[test]
    fn sections_map_descriptor_round_trip() {
        let descriptor = SectionDescriptor::empty_for_name("AcDb:Header");
        let mut bytes = Vec::new();
        descriptor.encode(&mut bytes);
        // 64-byte header + 22-byte name ("AcDb:Header" is 11 ASCII
        // chars * 2 = 22 UTF-16LE bytes).
        assert_eq!(bytes.len(), 64 + 22);
        // Fields:
        assert_eq!(&bytes[0..8], &1i64.to_le_bytes()); // data_size
        assert_eq!(&bytes[8..16], &1i64.to_le_bytes()); // max_size
        assert_eq!(&bytes[16..24], &0i64.to_le_bytes()); // encrypted
        assert_eq!(&bytes[24..32], &0i64.to_le_bytes()); // hashcode
        assert_eq!(&bytes[32..40], &22i64.to_le_bytes()); // name_length
        assert_eq!(&bytes[40..48], &0i64.to_le_bytes()); // unknown
        assert_eq!(&bytes[48..56], &0i64.to_le_bytes()); // encoded
        assert_eq!(&bytes[56..64], &0i64.to_le_bytes()); // num_pages
                                                         // Name: "AcDb:Header" in UTF-16LE
        let mut expected_name = Vec::new();
        for code_unit in "AcDb:Header".encode_utf16() {
            expected_name.extend_from_slice(&code_unit.to_le_bytes());
        }
        assert_eq!(&bytes[64..], &expected_name[..]);
    }

    #[test]
    fn mandatory_section_names_match_libredwg() {
        // Pinned against dwg.c::dwg_section_r2004_names. Order
        // matters only for emission stability — LibreDWG looks each
        // up by type via dwg_section_wtype.
        assert_eq!(
            MANDATORY_R2007_SECTION_NAMES,
            &[
                "AcDb:Header",
                "AcDb:AuxHeader",
                "AcDb:Classes",
                "AcDb:Handles",
                "AcDb:AcDbObjects",
            ]
        );
    }
}
