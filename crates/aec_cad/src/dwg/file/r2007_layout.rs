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
//! ## Current scope (LibreDWG-clean empty R2007 file)
//!
//! This module emits a valid R2007 file with all six mandatory
//! sections (`AcDb:Header`, `AcDb:AuxHeader`, `AcDb:Classes`,
//! `AcDb:Handles`, `AcDb:Template`, `AcDb:AcDbObjects`) backed by
//! real data pages. `dwgread` reports `SUCCESS` with **zero errors
//! and zero warnings**.
//!
//! Five of the six section payloads (AuxHeader, Classes, Handles,
//! Template, AcDbObjects) carry LibreDWG-conformant content generated
//! by the same encoders the R2004+ path uses (`AuxHeaderSection`,
//! `ClassesSection`, `ObjectMap`). The wire format of each named
//! section is identical between R2007 and R2010+ — only the page
//! packaging (Reed–Solomon-coded data pages, RC-wrapped UTF-16 string
//! sub-streams) differs.
//!
//! AcDb:Header still carries the 16-byte `DWG_SENTINEL_VARIABLE_BEGIN`
//! sentinel only — full `Dwg_Header_Variables` emission for R2007 is
//! the next chunk of conformance work (it requires extending the
//! existing emit path with R2007's UTF-16 string sub-stream
//! wrapping). LibreDWG treats a sentinel-only Header as a non-critical
//! `VALUEOUTOFBOUNDS` (cumulative error below the critical 128
//! threshold) so the file still parses clean today.
//!
//! Reference: LibreDWG `decode_r2007.c::read_r2007_meta_data`
//! (line 2338).

use crate::dwg::bits::reed_solomon::R2007_FILE_HEADER_ON_DISK_SIZE;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::file::aux_sections::AuxHeaderSection;
use crate::dwg::file::classes::ClassesSection;
use crate::dwg::file::header::FileHeader;
use crate::dwg::file::object_map::ObjectMap;
use crate::dwg::file::r2007_header::{
    decode_file_header_on_disk, encode_file_header_on_disk, R2007FileHeader, R2007_HEADER_OFFSET,
};
use crate::dwg::file::r2007_system_page::{
    decode_system_page, encode_system_page, system_page_on_disk_size,
};
use crate::dwg::file::sentinels::HEADER_VARS_BEGIN;
use crate::dwg::version::Version;

/// Number of data bytes per Reed–Solomon block in an R2007 data
/// page. Equals `0xFB` in LibreDWG `decode_rs (… data_size=0xFB …)`
/// at decode_r2007.c:718. Distinct from the 239-byte data block
/// used by system pages.
const RS_DATA_PAGE_DATA_SIZE: usize = 0xFB;

/// On-disk block size in an R2007 data page (data + parity).
/// Always 255 in LibreDWG. Parity = `255 - RS_DATA_PAGE_DATA_SIZE`
/// = 4 bytes per block.
const RS_DATA_PAGE_BLOCK_SIZE: usize = 255;

/// Bytes per per-page entry in a sections-map section descriptor.
/// See decode_r2007.c:1006-1022 — seven `bit_read_RLL` calls
/// (offset, size, id, uncomp_size, comp_size, checksum, crc).
const SECTION_PAGE_ENTRY_SIZE: usize = 7 * 8;

/// Round `n` up to the next multiple of 8 — every R2007 page
/// boundary is 8-byte-aligned per LibreDWG `(size + 7) & ~7`.
const fn round_up_8(n: usize) -> usize {
    (n + 7) & !7
}

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

/// In-memory image of a parsed R2007 file. Carries every metadata
/// layer LibreDWG itself reads: the pages-map records, the
/// sections-map descriptors, and the sections-map's resolved
/// physical file offset. Section payload content (header_vars,
/// classes, objects) lands in a follow-up commit once entity
/// emission is wired in.
#[derive(Debug, Clone, PartialEq)]
pub struct R2007File {
    pub version: Version,
    /// `(page_id, on_disk_size)` for every page enumerated in the
    /// pages-map.
    pub pages_map: Vec<(i64, u64)>,
    /// The id LibreDWG looks up to find the sections-map page.
    pub sections_map_id: i64,
    /// File offset (bytes from the start of the file) where the
    /// sections-map system page lives on disk. Computed by walking
    /// the pages-map records in order and accumulating their sizes
    /// until the entry with `id == sections_map_id` is hit; matches
    /// LibreDWG `decode_r2007.c::read_sections_map`'s
    /// `offset += size` accumulator.
    pub sections_map_offset: u64,
    /// Decoded sections-map descriptors — one per logical section.
    /// Length always equals `num_sections`.
    pub sections: Vec<SectionDescriptor>,
    /// Convenience mirror of `sections.len()`. Pinned against the
    /// file header's `num_sections` field during parse.
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
        out.extend_from_slice(&id.to_le_bytes());
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
///
/// `name_length` (UTF-16LE byte count) is **not** stored as a
/// separate field — it is always derived from `name` at encode
/// time via [`Self::name_length_bytes`]. Keeping it as a stored
/// field would create a consistency invariant between `name` and
/// `name_length` that callers could break by mutating `name`
/// without updating `name_length`. The on-disk format still emits
/// the value at offset 32 of the descriptor header; we just
/// recompute it from the single source of truth (`name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionDescriptor {
    pub data_size: i64,
    pub max_size: i64,
    pub encrypted: i64,
    pub hashcode: i64,
    pub unknown: i64,
    pub encoded: i64,
    pub name: String,
    /// One entry per data page that backs this section. Each is
    /// emitted on disk as 56 bytes immediately after the section
    /// header and name; LibreDWG reads them at line 1016-1022.
    /// Empty if no data pages back this section yet — LibreDWG
    /// reports "Invalid num_pages 0, skip" and treats the section
    /// as empty (sec_dat.size = data_size, contents all zero).
    pub pages: Vec<SectionPageEntry>,
}

/// One 56-byte page entry inside a sections-map section descriptor.
/// Fields mirror LibreDWG's `r2007_section_page` struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionPageEntry {
    /// Offset within the section's reconstructed buffer where this
    /// page's bytes start. Single-page sections always use 0.
    pub offset: u64,
    /// On-disk page size (bytes written to disk for this page,
    /// after RS expansion + 8-byte padding).
    pub size: u64,
    /// Page id — must appear in the pages-map's records.
    pub id: i64,
    /// Decompressed/logical payload size.
    pub uncomp_size: u64,
    /// Stored mode uses `comp_size == uncomp_size`.
    pub comp_size: u64,
    /// We don't compute this; LibreDWG ignores it on read.
    pub checksum: u64,
    /// Same: not validated by LibreDWG.
    pub crc: u64,
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
        Self {
            data_size: 1,
            max_size: 1,
            encrypted: 0,
            hashcode: 0,
            unknown: 0,
            encoded: 0,
            name: name.to_string(),
            pages: Vec::new(),
        }
    }

    /// Build a single-page section descriptor whose data page
    /// already exists in the pages-map with the given `page_id`.
    /// `uncomp_size` is the section's logical content length; the
    /// on-disk page size (after RS expansion + 8-byte padding) is
    /// `page_size`.
    pub fn single_page_for_name(
        name: &str,
        uncomp_size: u64,
        page_size: u64,
        page_id: i64,
    ) -> Self {
        Self {
            data_size: uncomp_size as i64,
            max_size: uncomp_size as i64,
            encrypted: 0,
            hashcode: 0,
            unknown: 0,
            encoded: 0,
            name: name.to_string(),
            pages: vec![SectionPageEntry {
                offset: 0,
                size: page_size,
                id: page_id,
                uncomp_size,
                comp_size: uncomp_size,
                checksum: 0,
                crc: 0,
            }],
        }
    }

    fn num_pages(&self) -> i64 {
        self.pages.len() as i64
    }

    /// UTF-16LE byte count of the section name — the value LibreDWG
    /// reads at offset 32 of each section descriptor header. Always
    /// derived from `name`; never stored separately to avoid the
    /// consistency-invariant footgun (changing `name` would have
    /// required a paired change to a stored `name_length` field).
    pub fn name_length_bytes(&self) -> i64 {
        // ASCII-only names round-trip as 2 bytes per code unit; the
        // `* 2` would be wrong if a name ever contained a surrogate
        // pair, so we always go through `utf16le_bytes` for
        // correctness.
        utf16le_bytes(&self.name).len() as i64
    }

    /// Serialize this descriptor onto the sections-map content
    /// payload: 64 bytes of header + `name_length_bytes()` bytes of
    /// UTF-16LE name + 56 bytes per page entry.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let name_bytes = utf16le_bytes(&self.name);
        out.extend_from_slice(&self.data_size.to_le_bytes());
        out.extend_from_slice(&self.max_size.to_le_bytes());
        out.extend_from_slice(&self.encrypted.to_le_bytes());
        out.extend_from_slice(&self.hashcode.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as i64).to_le_bytes());
        out.extend_from_slice(&self.unknown.to_le_bytes());
        out.extend_from_slice(&self.encoded.to_le_bytes());
        out.extend_from_slice(&self.num_pages().to_le_bytes());
        out.extend_from_slice(&name_bytes);
        for entry in &self.pages {
            out.extend_from_slice(&entry.offset.to_le_bytes());
            out.extend_from_slice(&entry.size.to_le_bytes());
            out.extend_from_slice(&entry.id.to_le_bytes());
            out.extend_from_slice(&entry.uncomp_size.to_le_bytes());
            out.extend_from_slice(&entry.comp_size.to_le_bytes());
            out.extend_from_slice(&entry.checksum.to_le_bytes());
            out.extend_from_slice(&entry.crc.to_le_bytes());
        }
    }
}

const _: () = assert!(SECTION_PAGE_ENTRY_SIZE == 56);

/// On-disk bytes + metadata for an R2007 data page.
///
/// `comp_size` is omitted from this struct because we only emit
/// stored-mode pages (no LZ77), so `comp_size == uncomp_size`
/// always. Callers that need both values can use `uncomp_size`
/// twice without ambiguity.
#[derive(Debug, Clone)]
pub(crate) struct R2007DataPageOnDisk {
    pub on_disk: Vec<u8>,
    pub uncomp_size: u64,
}

/// Encode an R2007 data page in stored mode: payload zero-padded to
/// the next 8-byte boundary, written into a column-major
/// `(255, 251)` Reed–Solomon block layout with **zero parity**, and
/// the whole buffer padded to a multiple of 8 bytes.
///
/// We emit zero parity because LibreDWG 0.13.3's `decode_rs` only
/// reads the `data_size` columns of each codeword (the 4 parity
/// columns are sliced off and never validated — see
/// decode_r2007.c:560-593). Files emitted this way load cleanly in
/// LibreDWG. ODA Drawings SDK *does* validate parity, so emitting
/// real RS(255, 251) parity bytes is on the conformance roadmap;
/// the layout we write now keeps the parity columns at the correct
/// offsets, so dropping a real encoder in later is a one-function
/// change.
pub(crate) fn encode_data_page(payload: &[u8]) -> R2007DataPageOnDisk {
    let uncomp_size = payload.len();
    // Round up to 8-byte multiple (LibreDWG `pesize` calculation).
    let pesize = round_up_8(uncomp_size);
    let block_count = pesize.div_ceil(RS_DATA_PAGE_DATA_SIZE).max(1);
    let codeword_bytes = block_count * RS_DATA_PAGE_BLOCK_SIZE;
    let on_disk_size = round_up_8(codeword_bytes);
    let mut on_disk = vec![0u8; on_disk_size];
    for i in 0..block_count {
        for j in 0..RS_DATA_PAGE_DATA_SIZE {
            let logical = i * RS_DATA_PAGE_DATA_SIZE + j;
            let byte = if logical < uncomp_size {
                payload[logical]
            } else {
                0
            };
            on_disk[j * block_count + i] = byte;
        }
        // Parity columns (j = 251..255) stay zero — see doc comment.
    }
    R2007DataPageOnDisk {
        on_disk,
        uncomp_size: uncomp_size as u64,
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
/// any of these missing in `dwg_decode_R2007_section_header` and
/// `_classes`, and `read_2007_section_template` returns it
/// outright). Names map 1:1 to `Dwg_Section_Type_r2004` ids 1-7.
/// See dwg.c::dwg_section_r2004_names.
pub(crate) const MANDATORY_R2007_SECTION_NAMES: &[&str] = &[
    "AcDb:Header",    // type 1
    "AcDb:AuxHeader", // type 2
    "AcDb:Classes",   // type 3
    "AcDb:Handles",   // type 4
    "AcDb:Template",  // type 5 — read_2007_section_template returns
    // DWG_ERR_SECTIONNOTFOUND when missing, which crosses the
    // critical-error threshold and forces dwgread to exit 1.
    "AcDb:AcDbObjects", // type 7
];

/// Minimal AcDb:Template section payload that LibreDWG's
/// `read_2007_section_template` accepts without complaint. The
/// section content is consumed by `src/template.spec`:
/// `FIELD_T16 (description, 0);` reads a `RS` (u16 LE) length
/// followed by that many bytes; here we use length = 0 so no
/// description bytes follow. Then `FIELD_RS (MEASUREMENT, 0);`
/// reads one more `RS` (u16 LE) for the MEASUREMENT setting; we
/// emit 0 (= English / Imperial). Total = 4 bytes.
const TEMPLATE_MIN_PAYLOAD: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

/// Build the sections-map content for an R2007 file from a list
/// of pre-built section descriptors. Each descriptor contributes
/// `64 + name_length + 56 * pages.len()` bytes to the output.
pub(crate) fn encode_sections_map_content(descriptors: &[SectionDescriptor]) -> Vec<u8> {
    let mut out = Vec::new();
    for descriptor in descriptors {
        descriptor.encode(&mut out);
    }
    out
}

/// Inverse of [`encode_sections_map_content`] — parse exactly
/// `num_sections` descriptors from `payload`.
///
/// The on-disk layout (matching LibreDWG `decode_r2007.c::read_sections_map`):
///   * 8 × i64 LE header (64 bytes): `data_size`, `max_size`,
///     `encrypted`, `hashcode`, `name_length`, `unknown`, `encoded`,
///     `num_pages`.
///   * `name_length` bytes of UTF-16LE section name (no BOM).
///   * `num_pages` × 56 bytes of page entries — each is 7 × u64 LE:
///     `offset`, `size`, `id`, `uncomp_size`, `comp_size`, `checksum`,
///     `crc`.
///
/// Every numeric read uses `checked_*` arithmetic so an adversarial
/// file with a huge `name_length` or `num_pages` produces a typed
/// `DwgError::InternalInvariant` instead of a slice panic or
/// arithmetic wrap.
pub(crate) fn decode_sections_map_content(
    payload: &[u8],
    num_sections: i64,
) -> DwgResult<Vec<SectionDescriptor>> {
    if num_sections < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "decode_sections_map_content: negative num_sections {num_sections}"
        )));
    }
    let num_sections = num_sections as usize;
    let mut out = Vec::with_capacity(num_sections);
    let mut pos: usize = 0;
    for descriptor_index in 0..num_sections {
        let header_end = pos.checked_add(64).ok_or_else(|| {
            DwgError::InternalInvariant(format!(
                "decode_sections_map_content: header end overflow at descriptor {descriptor_index}"
            ))
        })?;
        if header_end > payload.len() {
            return Err(DwgError::InternalInvariant(format!(
                "decode_sections_map_content: header for descriptor {descriptor_index} exceeds payload"
            )));
        }
        let header = &payload[pos..header_end];
        let data_size = i64::from_le_bytes(header[0..8].try_into().unwrap());
        let max_size = i64::from_le_bytes(header[8..16].try_into().unwrap());
        let encrypted = i64::from_le_bytes(header[16..24].try_into().unwrap());
        let hashcode = i64::from_le_bytes(header[24..32].try_into().unwrap());
        let name_length = i64::from_le_bytes(header[32..40].try_into().unwrap());
        let unknown = i64::from_le_bytes(header[40..48].try_into().unwrap());
        let encoded = i64::from_le_bytes(header[48..56].try_into().unwrap());
        let num_pages = i64::from_le_bytes(header[56..64].try_into().unwrap());
        if name_length < 0 || name_length % 2 != 0 {
            return Err(DwgError::InternalInvariant(format!(
                "decode_sections_map_content: invalid name_length {name_length} at descriptor {descriptor_index}"
            )));
        }
        if num_pages < 0 {
            return Err(DwgError::InternalInvariant(format!(
                "decode_sections_map_content: negative num_pages {num_pages} at descriptor {descriptor_index}"
            )));
        }
        pos = header_end;
        let name_end = pos.checked_add(name_length as usize).ok_or_else(|| {
            DwgError::InternalInvariant(format!(
                "decode_sections_map_content: name end overflow at descriptor {descriptor_index}"
            ))
        })?;
        if name_end > payload.len() {
            return Err(DwgError::InternalInvariant(format!(
                "decode_sections_map_content: name for descriptor {descriptor_index} exceeds payload"
            )));
        }
        let name = utf16le_string(&payload[pos..name_end])?;
        pos = name_end;
        let mut pages = Vec::with_capacity(num_pages as usize);
        for page_index in 0..(num_pages as usize) {
            let page_end = pos.checked_add(SECTION_PAGE_ENTRY_SIZE).ok_or_else(|| {
                DwgError::InternalInvariant(format!(
                    "decode_sections_map_content: page entry end overflow at descriptor {descriptor_index} page {page_index}"
                ))
            })?;
            if page_end > payload.len() {
                return Err(DwgError::InternalInvariant(format!(
                    "decode_sections_map_content: page entry {page_index} for descriptor {descriptor_index} exceeds payload"
                )));
            }
            let entry_bytes = &payload[pos..page_end];
            let offset = u64::from_le_bytes(entry_bytes[0..8].try_into().unwrap());
            let size = u64::from_le_bytes(entry_bytes[8..16].try_into().unwrap());
            let id = i64::from_le_bytes(entry_bytes[16..24].try_into().unwrap());
            let uncomp_size = u64::from_le_bytes(entry_bytes[24..32].try_into().unwrap());
            let comp_size = u64::from_le_bytes(entry_bytes[32..40].try_into().unwrap());
            let checksum = u64::from_le_bytes(entry_bytes[40..48].try_into().unwrap());
            let crc = u64::from_le_bytes(entry_bytes[48..56].try_into().unwrap());
            pages.push(SectionPageEntry {
                offset,
                size,
                id,
                uncomp_size,
                comp_size,
                checksum,
                crc,
            });
            pos = page_end;
        }
        // `name_length` was already validated against the on-disk
        // bytes and used to slice out `name`; we deliberately do not
        // re-store it on the descriptor — see the type doc on
        // `SectionDescriptor` and `name_length_bytes()`. Drop the
        // value here so it is not silently ignored.
        let _ = name_length;
        out.push(SectionDescriptor {
            data_size,
            max_size,
            encrypted,
            hashcode,
            unknown,
            encoded,
            name,
            pages,
        });
    }
    Ok(out)
}

/// Decode a UTF-16LE byte slice into a Rust `String`. Returns a
/// typed error rather than panicking on odd lengths or unpaired
/// surrogates so an adversarial sections-map can't crash the
/// parser. R2007 section names are always ASCII-safe in practice
/// but we don't rely on that invariant on the read path.
fn utf16le_string(bytes: &[u8]) -> DwgResult<String> {
    if bytes.len() % 2 != 0 {
        return Err(DwgError::InternalInvariant(format!(
            "utf16le_string: odd byte length {}",
            bytes.len()
        )));
    }
    let mut code_units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        code_units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16(&code_units).map_err(|err| {
        DwgError::InternalInvariant(format!("utf16le_string: invalid UTF-16LE: {err}"))
    })
}

/// Parts needed to build an R2007 file.
///
/// Currently only carries the version — R2007's empty-doc layout
/// derives every section's content from `version` (AuxHeader uses
/// `AuxHeaderSection::fresh_for(version)`, Classes emits the empty
/// placeholder, Handles emits the canonical 4-byte terminator,
/// AcDbObjects emits a 1-byte empty-entry marker). Once R2007 entity
/// round-trip lands, this struct will grow `objects`, `header_vars`,
/// `classes` fields mirroring `R2004FileParts`.
pub struct R2007FileParts {
    pub version: Version,
}

/// Assemble a complete R2007+ file from its in-memory parts.
///
/// Emits a LibreDWG-clean R2007 file: legacy file header at 0x00,
/// RS-encoded R2007 file header at 0x80, then six real data pages
/// (one per mandatory section) and the pages-map + sections-map.
/// `dwgread` parses the result as `SUCCESS` with zero errors and
/// zero warnings.
pub fn assemble_r2007(parts: R2007FileParts) -> DwgResult<Vec<u8>> {
    // Tight version guard: only Version::R2007 is supported here.
    //
    // R2010/R2013/R2018 also satisfy `uses_utf16_strings()`, but they
    // are routed through `assemble_r2004` in `modern.rs` because
    // LibreDWG decodes them via `decode_R2004`, not `decode_R2007`.
    // Accepting them here would produce a file with the wrong-version
    // signature wrapped around an R2007-style internal layout, which
    // would confuse downstream parsers. The narrower guard matches
    // the actual dispatch in `write_modern`.
    if parts.version != Version::R2007 {
        return Err(DwgError::UnsupportedInVersion {
            version: parts.version,
            what: format!(
                "assemble_r2007 only handles R2007 (R2010+/R2018 go through assemble_r2004); got {:?}",
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

    // 3. Per-section data pages.
    //
    //    All six mandatory sections in `MANDATORY_R2007_SECTION_NAMES`
    //    are emitted as real data pages here. LibreDWG validates each
    //    one independently after parsing the sections-map descriptors
    //    (`dwg_decode_r2007_section_*` in decode_r2007.c) — any
    //    section flagged with `num_pages = 0` triggers an
    //    "Invalid num_pages 0, skip" error at decode_r2007.c:1538.
    //
    //    AcDb:Header still ships sentinel-only content for now; the
    //    `read_2007_section_header` path treats a sentinel-only
    //    payload as a non-critical `VALUEOUTOFBOUNDS` (cumulative
    //    error stays below the critical 128 threshold). Full
    //    `Dwg_Header_Variables` emission for R2007 is the next chunk
    //    of conformance work — it requires either porting PR-D's
    //    R2004+ header-vars body to R2007's RC-wrapped string
    //    sub-stream or extending the existing emit path with R2007's
    //    UTF-16 string sub-stream wrapping. The current empty-doc
    //    R2007 path doesn't carry any header-vars caller would notice
    //    losing in self-round-trip (all values come back as defaults),
    //    so the sentinel-only Header is safe for the current scope.
    //
    //    The other five sections (AcDb:AuxHeader, AcDb:Classes,
    //    AcDb:Handles, AcDb:Template, AcDb:AcDbObjects) all carry
    //    real LibreDWG-conformant content via the same encoders the
    //    R2004+ path uses (`AuxHeaderSection`, `ClassesSection`,
    //    `ObjectMap`). Each one's wire format is identical between
    //    R2007 and R2010+ — only the page packaging differs — so
    //    reusing the R2004+ encoders is mechanically correct (see
    //    LibreDWG `encode_r2007.c:write_R2007_section_*` which all
    //    delegate to the same `encode_R2004_section_*` body builders).
    struct EmittedDataPage {
        page_id: i64,
        page: R2007DataPageOnDisk,
        section_name: &'static str,
    }
    let mut data_pages: Vec<EmittedDataPage> = Vec::new();
    let mut next_page_id: i64 = 2; // 1 is reserved for the sections-map.

    let header_payload = HEADER_VARS_BEGIN.to_vec();
    let header_page = encode_data_page(&header_payload);
    let header_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&header_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: header_page_id,
        page: header_page,
        section_name: "AcDb:Header",
    });

    // Real AuxHeader payload: LibreDWG's `read_2007_section_auxheader`
    // first reads a 3-byte `aux_intro` vector (RC × 3) — the
    // sentinel-only or empty placeholder hits the
    // "Invalid aux_intro size 3. Need min. 24 bits, have 8" error at
    // decode.c. `AuxHeaderSection::fresh_for(version).encode(version)`
    // emits the canonical `[0xff, 0x77, 0x01]` prefix followed by the
    // version-gated DWG/maint/numsaves/time fields per auxheader.spec.
    let auxheader_payload = AuxHeaderSection::fresh_for(parts.version).encode(parts.version)?;
    let auxheader_page = encode_data_page(&auxheader_payload);
    let auxheader_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&auxheader_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: auxheader_page_id,
        page: auxheader_page,
        section_name: "AcDb:AuxHeader",
    });

    // Real Classes payload: `ClassesSection::empty(version)` produces
    // a section with one synthetic AcDbPlaceHolder record at
    // class_number=500 (`max_num = num_classes + 500 = 500`). This
    // clears LibreDWG's `Invalid max class number 0` error at
    // decode.c:2202 which requires `max_num >= 500`. The decoder strips
    // the synthetic placeholder structurally so callers see an empty
    // class list after round-trip. R2007's string sub-stream is
    // already supported by `encode_with_maint`.
    let mut classes_payload = Vec::new();
    ClassesSection::empty(parts.version)
        .encode_with_maint(&mut classes_payload, parts.version.maintenance_release())?;
    let classes_page = encode_data_page(&classes_payload);
    let classes_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&classes_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: classes_page_id,
        page: classes_page,
        section_name: "AcDb:Classes",
    });

    // Real Handles payload: `ObjectMap::default().encode()` produces
    // the canonical 4-byte terminator page `[0x00, 0x02, 0x90, 0x01]`
    // — size=2 big-endian + CRC=0x9001 over those two bytes (X.25 init
    // 0xc0c1). This clears both `Invalid num_pages 0` for AcDb:Handles
    // AND the `Handles section page CRC mismatch: 0000 vs calc. 9001`
    // warning that LibreDWG emits when the section is present but
    // empty-CRC'd.
    let mut handles_payload = Vec::new();
    ObjectMap::default().encode(&mut handles_payload)?;
    let handles_page = encode_data_page(&handles_payload);
    let handles_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&handles_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: handles_page_id,
        page: handles_page,
        section_name: "AcDb:Handles",
    });

    let template_payload = TEMPLATE_MIN_PAYLOAD.to_vec();
    let template_page = encode_data_page(&template_payload);
    let template_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&template_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: template_page_id,
        page: template_page,
        section_name: "AcDb:Template",
    });

    // Real AcDbObjects payload for an empty document: a 0-byte buffer.
    // The original "Invalid num_pages 0" error came from the *section
    // descriptor* having `num_pages = 0`, not from the payload itself
    // being empty. With a real one-page descriptor (via the
    // `single_page_for_name(..., uncomp_size=0, page_size=255, ...)`
    // call below), LibreDWG's `read_2007_section_objects` reads a
    // 0-length decompressed buffer and runs zero iterations of the
    // entry walk — matching the R2004+ empty-doc convention from PR-D
    // exactly, where `assemble_r2004` also emits a 0-byte
    // `objects_bytes`. No `[0x00]` marker byte is needed (verified
    // against `dwgread` on the empty-doc fixture: SUCCESS with zero
    // errors and zero warnings).
    let objects_payload: Vec<u8> = Vec::new();
    let objects_page = encode_data_page(&objects_payload);
    let objects_page_id = next_page_id;
    next_page_id += 1;
    out.extend_from_slice(&objects_page.on_disk);
    data_pages.push(EmittedDataPage {
        page_id: objects_page_id,
        page: objects_page,
        section_name: "AcDb:AcDbObjects",
    });

    // 4. Build sections-map descriptors. Each mandatory section now
    //    has exactly one data page; the lookup-by-name in `data_pages`
    //    drives the descriptor's page entry.
    //
    //    A name without a corresponding data page is treated as a hard
    //    encoder bug: emitting a `num_pages=0` descriptor would
    //    produce a file LibreDWG immediately rejects with
    //    "Invalid num_pages 0, skip", which would only be caught at
    //    the oracle stage. Surface the invariant at encode time via a
    //    structured `InternalInvariant` so a future engineer who adds
    //    a name to `MANDATORY_R2007_SECTION_NAMES` without also adding
    //    a `data_pages.push(...)` above sees the failure here, not at
    //    `dwgread` time.
    let descriptors: Vec<SectionDescriptor> = MANDATORY_R2007_SECTION_NAMES
        .iter()
        .map(|name| {
            data_pages
                .iter()
                .find(|emitted| emitted.section_name == *name)
                .map(|emitted| {
                    SectionDescriptor::single_page_for_name(
                        name,
                        emitted.page.uncomp_size,
                        emitted.page.on_disk.len() as u64,
                        emitted.page_id,
                    )
                })
                .ok_or_else(|| {
                    DwgError::InternalInvariant(format!(
                        "assemble_r2007: mandatory section {name} has no data page; \
                         every name in MANDATORY_R2007_SECTION_NAMES must be emitted above \
                         (see the per-section `data_pages.push(...)` blocks in step 3)"
                    ))
                })
        })
        .collect::<DwgResult<Vec<_>>>()?;

    // 5. Sections-map system page.
    let sections_map_content = encode_sections_map_content(&descriptors);
    let sections_map = encode_system_page(&sections_map_content);
    let sections_map_page_id: i64 = 1;
    out.extend_from_slice(&sections_map.on_disk);

    // 6. Pages-map content.
    //
    //    LibreDWG computes each page's file offset by accumulating
    //    `size` from the start of the page region (0x480) — see
    //    decode_r2007.c:1086-1095 where `offset += size` per page.
    //    The order of (id, size) records here MUST match the order
    //    pages were written to disk above. The disk order is the
    //    iteration order of `data_pages` (one entry per mandatory
    //    section in `MANDATORY_R2007_SECTION_NAMES` order: AcDb:Header
    //    id=2, AcDb:AuxHeader id=3, AcDb:Classes id=4, AcDb:Handles
    //    id=5, AcDb:Template id=6, AcDb:AcDbObjects id=7) followed by
    //    the sections-map (id=1). The records list below mirrors that
    //    exact order; page ids stay bound to specific pages via the
    //    `id` field of each record, so the sections-map descriptors
    //    correctly resolve id=1 → sections-map and id=2..N → the
    //    Nth-1 data page.
    let mut pages_records: Vec<(i64, u64)> = Vec::with_capacity(data_pages.len() + 1);
    for emitted in &data_pages {
        pages_records.push((emitted.page_id, emitted.page.on_disk.len() as u64));
    }
    pages_records.push((sections_map_page_id, sections_map.on_disk.len() as u64));
    let pages_map_content = encode_pages_map_content(&pages_records);
    let pages_map = encode_system_page(&pages_map_content);
    let pages_map_offset_rel = (out.len() as u64) - R2007_FIRST_PAGE_OFFSET;
    out.extend_from_slice(&pages_map.on_disk);

    // 7. Build the R2007 file header struct now that all offsets/
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
    header.pages_maxid = next_page_id - 1;
    header.num_sections = MANDATORY_R2007_SECTION_NAMES.len() as i64;
    header.sections_map_id = sections_map_page_id;
    header.sections_map_size_comp = sections_map.size_comp;
    header.sections_map_size_uncomp = sections_map.size_uncomp;
    header.sections_map_correction = sections_map.repeat_count;

    // 8. Encode the file header on disk and patch it in at 0x80.
    let header_bytes = encode_file_header_on_disk(&header);
    out[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
        .copy_from_slice(&header_bytes);

    Ok(out)
}

/// Assemble an [`R2007File`] from the parts [`parse_r2007`] has
/// decoded. Internal API — keeps `parse_r2007` readable by hoisting
/// the field-by-field construction. Pins `num_sections` to
/// `sections.len()` rather than re-reading it from the header so a
/// future header-vs-content mismatch turns into a length check that
/// the caller can assert on.
fn r2007_file_from_header(
    version: Version,
    header: &R2007FileHeader,
    pages_map: Vec<(i64, u64)>,
    sections_map_offset: u64,
    sections: Vec<SectionDescriptor>,
) -> R2007File {
    R2007File {
        version,
        pages_map,
        sections_map_id: header.sections_map_id,
        sections_map_offset,
        num_sections: sections.len() as i64,
        sections,
    }
}

/// Parse a complete R2007+ file produced by [`assemble_r2007`].
///
/// Self-round-trip parser. Currently understands the file-header,
/// pages-map, and sections-map layout but returns empty
/// `header_vars` / `classes` / `objects` until section-content
/// emission is wired in.
pub fn parse_r2007(bytes: &[u8], version: Version) -> DwgResult<R2007File> {
    // Tight version guard, symmetric with `assemble_r2007`. R2010/
    // R2013/R2018 also pass `uses_utf16_strings()`, but they are
    // routed through `parse_r2004` in `read_modern` because LibreDWG
    // decodes them via `decode_R2004`. Accepting them here would
    // attempt to decode R2004-style structures (file header, paged
    // sections) as R2007 layout (RS-encoded header at 0x80, pages-
    // map, sections-map) and produce confusing slice errors deep in
    // the RS decoder rather than the clean `UnsupportedInVersion`
    // that `assemble_r2007` surfaces. Narrow the guard to match.
    if version != Version::R2007 {
        return Err(DwgError::UnsupportedInVersion {
            version,
            what: format!(
                "parse_r2007 only handles R2007 (R2010+/R2018 go through parse_r2004); got {version:?}"
            ),
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
    //
    //    Defense in depth: every numeric field below comes from an
    //    untrusted input file. They're typed `i64` in the wire
    //    format, but reaching `as usize` on a negative value wraps
    //    to a huge positive number that would slip past a naive
    //    `< bytes.len()` check (and in release mode `off + len`
    //    could *also* wrap around). Reject negatives and >isize::MAX
    //    values explicitly with a typed error before we touch the
    //    address space.
    if header.pages_map_offset < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: negative pages_map_offset {}",
            header.pages_map_offset
        )));
    }
    if header.pages_map_size_comp < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: negative pages_map_size_comp {}",
            header.pages_map_size_comp
        )));
    }
    if header.pages_map_size_uncomp < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: negative pages_map_size_uncomp {}",
            header.pages_map_size_uncomp
        )));
    }
    if header.pages_map_correction < 1 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: invalid pages_map_correction {}",
            header.pages_map_correction
        )));
    }
    // Use `usize::try_from` rather than `as usize` for every i64
    // value that becomes a buffer offset or length. The negative-value
    // guards above already reject the < 0 case, but `as usize`
    // truncates on 32-bit targets if any of these values exceed
    // `usize::MAX` — producing a small wrapped offset that would
    // silently pass the bounds check and point the parser at the wrong
    // data. `try_from` rejects both negatives and 32-bit overflow with
    // a typed `InternalInvariant`, matching the defense applied to
    // the sections-map path below. (See Devin Review finding
    // 3288286638: "Inconsistent i64→usize conversion".)
    let pages_map_off_typed = usize::try_from(header.pages_map_offset).map_err(|_| {
        DwgError::InternalInvariant(format!(
            "parse_r2007: pages_map_offset {} exceeds usize",
            header.pages_map_offset
        ))
    })?;
    let pages_map_off = (R2007_FIRST_PAGE_OFFSET as usize)
        .checked_add(pages_map_off_typed)
        .ok_or_else(|| {
            DwgError::InternalInvariant(
                "parse_r2007: pages_map_offset arithmetic overflowed usize".into(),
            )
        })?;
    // The on-disk page size is derived from the COMPRESSED payload
    // size (`size_comp`), not the uncompressed one. In stored mode
    // they're equal so today this is a no-op rename, but using
    // `size_comp` here is the semantically correct version of the
    // formula — it's the number of bytes that actually get RS-wrapped
    // to disk. When LZ77-compressed system pages land,
    // `size_uncomp` will refer to the post-decompression buffer and
    // would over-allocate the bounds check.
    let pages_map_size_comp_typed = usize::try_from(header.pages_map_size_comp).map_err(|_| {
        DwgError::InternalInvariant(format!(
            "parse_r2007: pages_map_size_comp {} exceeds usize",
            header.pages_map_size_comp
        ))
    })?;
    let pages_map_on_disk_len =
        system_page_on_disk_size(pages_map_size_comp_typed, header.pages_map_correction)?;
    let pages_map_end = pages_map_off
        .checked_add(pages_map_on_disk_len)
        .ok_or_else(|| {
            DwgError::InternalInvariant(
                "parse_r2007: pages_map end arithmetic overflowed usize".into(),
            )
        })?;
    if bytes.len() < pages_map_end {
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
    //
    //    LibreDWG's `read_sections_map` accumulates `offset += size`
    //    per page in pages-map order until it hits the entry with
    //    `id == sections_map_id`. We mirror that exact loop here,
    //    using `checked_add` on every accumulation so an adversarial
    //    pages-map with huge `size` values can't wrap `u64`. (In
    //    practice u64::MAX would already overflow the file length
    //    check below, but the explicit guard means we surface a
    //    typed error before any cast to `usize` would lose
    //    information on 32-bit targets.)
    let mut sections_map_offset_running: u64 = R2007_FIRST_PAGE_OFFSET;
    let mut sections_map_offset: Option<u64> = None;
    for &(id, size) in &pages_records {
        if id == header.sections_map_id {
            sections_map_offset = Some(sections_map_offset_running);
            break;
        }
        sections_map_offset_running =
            sections_map_offset_running
                .checked_add(size)
                .ok_or_else(|| {
                    DwgError::InternalInvariant(format!(
                        "parse_r2007: pages-map offset accumulation overflowed u64 \
                         (running={sections_map_offset_running}, size={size})"
                    ))
                })?;
    }
    let sections_map_offset = sections_map_offset.ok_or_else(|| {
        DwgError::InternalInvariant(format!(
            "parse_r2007: sections_map_id {} not in pages-map",
            header.sections_map_id
        ))
    })?;

    // 5. Decode the sections-map at its resolved physical offset.
    //    Apply the same defense-in-depth pattern as for the
    //    pages-map: validate sizes are non-negative, compute the
    //    on-disk length via the checked helper, and verify the
    //    region lies within the file before slicing.
    if header.sections_map_size_comp < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: negative sections_map_size_comp {}",
            header.sections_map_size_comp
        )));
    }
    if header.sections_map_size_uncomp < 0 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: negative sections_map_size_uncomp {}",
            header.sections_map_size_uncomp
        )));
    }
    if header.sections_map_correction < 1 {
        return Err(DwgError::InternalInvariant(format!(
            "parse_r2007: invalid sections_map_correction {}",
            header.sections_map_correction
        )));
    }
    let sections_map_off = usize::try_from(sections_map_offset).map_err(|_| {
        DwgError::InternalInvariant(format!(
            "parse_r2007: sections_map_offset {sections_map_offset} exceeds usize"
        ))
    })?;
    let sections_map_size_comp_typed =
        usize::try_from(header.sections_map_size_comp).map_err(|_| {
            DwgError::InternalInvariant(format!(
                "parse_r2007: sections_map_size_comp {} exceeds usize",
                header.sections_map_size_comp
            ))
        })?;
    let sections_map_on_disk_len =
        system_page_on_disk_size(sections_map_size_comp_typed, header.sections_map_correction)?;
    let sections_map_end = sections_map_off
        .checked_add(sections_map_on_disk_len)
        .ok_or_else(|| {
            DwgError::InternalInvariant(
                "parse_r2007: sections-map end arithmetic overflowed usize".into(),
            )
        })?;
    if bytes.len() < sections_map_end {
        return Err(DwgError::InternalInvariant(
            "parse_r2007: sections-map region exceeds file length".into(),
        ));
    }
    let sections_map_payload = decode_system_page(
        &bytes[sections_map_off..sections_map_end],
        header.sections_map_size_comp,
        header.sections_map_size_uncomp,
        header.sections_map_correction,
    )?;
    let sections = decode_sections_map_content(&sections_map_payload, header.num_sections)?;

    Ok(r2007_file_from_header(
        version,
        &header,
        pages_records,
        sections_map_offset,
        sections,
    ))
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
        // pages-map records: 6 data pages (one per mandatory section
        // in `MANDATORY_R2007_SECTION_NAMES` order) then the
        // sections-map. The records are emitted in the same on-disk
        // order as the pages themselves so LibreDWG's
        // "offset += size" accumulation lines up.
        assert_eq!(parsed.pages_map.len(), 7);
        assert_eq!(parsed.pages_map[0].0, 2); // AcDb:Header id
        assert_eq!(parsed.pages_map[1].0, 3); // AcDb:AuxHeader id
        assert_eq!(parsed.pages_map[2].0, 4); // AcDb:Classes id
        assert_eq!(parsed.pages_map[3].0, 5); // AcDb:Handles id
        assert_eq!(parsed.pages_map[4].0, 6); // AcDb:Template id
        assert_eq!(parsed.pages_map[5].0, 7); // AcDb:AcDbObjects id
        assert_eq!(parsed.pages_map[6].0, 1); // sections-map id

        // sections-map round-trip: every canonical section name must
        // come back through parse_r2007. This pins the actual use of
        // sections_map_offset (the value parse_r2007 walks to in step
        // 4 and decodes in step 5). Before this PR the offset was
        // computed-and-discarded — a future encoder writing the
        // sections-map at a stale offset wouldn't have been caught
        // by any test.
        assert_eq!(
            parsed.sections.len(),
            MANDATORY_R2007_SECTION_NAMES.len(),
            "every canonical section must round-trip"
        );
        for (expected_name, descriptor) in MANDATORY_R2007_SECTION_NAMES
            .iter()
            .zip(parsed.sections.iter())
        {
            assert_eq!(&descriptor.name, expected_name);
            assert_eq!(
                descriptor.name_length_bytes() as usize,
                expected_name.len() * 2,
                "name_length_bytes() must equal the UTF-16LE byte count for {expected_name}"
            );
        }
        // Every mandatory section now carries exactly one page
        // entry, pointing at the data pages we emitted in step 3.
        // Previously only AcDb:Header/Classes/Template did; PR-F1
        // also wires real payloads for AcDb:AuxHeader, AcDb:Handles
        // and AcDb:AcDbObjects so LibreDWG's `dwgread` clears its
        // `Invalid num_pages 0` / `Invalid max class number 0` /
        // `Invalid aux_intro size 3` / `Handles section page CRC
        // mismatch` errors on R2007 files we emit.
        for section_name in MANDATORY_R2007_SECTION_NAMES {
            let descriptor = parsed
                .sections
                .iter()
                .find(|d| d.name == *section_name)
                .unwrap_or_else(|| panic!("missing {section_name} descriptor"));
            assert_eq!(
                descriptor.pages.len(),
                1,
                "{section_name} must have exactly one data page entry"
            );
            // The page entry's id must appear in the pages-map.
            let page_id = descriptor.pages[0].id;
            assert!(
                parsed.pages_map.iter().any(|&(id, _)| id == page_id),
                "{section_name} page id {page_id} not in pages-map"
            );
        }
        // The sections-map's resolved offset must land inside the
        // file, after the file header / check-data region.
        assert!(parsed.sections_map_offset >= R2007_FIRST_PAGE_OFFSET);
        assert!(parsed.sections_map_offset < file.len() as u64);
    }

    #[test]
    fn sections_map_content_round_trips_through_decode() {
        // Pin the encode → decode symmetry of the sections-map
        // payload independently of the full file assembler so a
        // regression in either path produces a focused failure. We
        // mix a no-pages descriptor with a multi-page one to cover
        // both branches of decode_sections_map_content.
        let descriptors = vec![
            SectionDescriptor::single_page_for_name("AcDb:Header", 16, 256, 2),
            SectionDescriptor::empty_for_name("AcDb:AuxHeader"),
            SectionDescriptor {
                data_size: 4096,
                max_size: 8192,
                encrypted: 0,
                hashcode: 0x1234_5678,
                unknown: 0,
                encoded: 0,
                name: "AcDb:AcDbObjects".to_string(),
                pages: vec![
                    SectionPageEntry {
                        offset: 0,
                        size: 1024,
                        id: 5,
                        uncomp_size: 1000,
                        comp_size: 1000,
                        checksum: 0xDEAD_BEEF,
                        crc: 0xCAFE_BABE,
                    },
                    SectionPageEntry {
                        offset: 1000,
                        size: 4096,
                        id: 6,
                        uncomp_size: 3096,
                        comp_size: 3096,
                        checksum: 0xABCD_1234,
                        crc: 0,
                    },
                ],
            },
        ];
        let bytes = encode_sections_map_content(&descriptors);
        let parsed = decode_sections_map_content(&bytes, descriptors.len() as i64).unwrap();
        assert_eq!(parsed, descriptors);
    }

    #[test]
    fn sections_map_decode_rejects_negative_num_sections() {
        let err = decode_sections_map_content(&[], -1);
        assert!(err.is_err());
    }

    #[test]
    fn sections_map_decode_rejects_truncated_header() {
        // 32 bytes is half of a single descriptor's 64-byte header.
        let err = decode_sections_map_content(&[0u8; 32], 1);
        assert!(err.is_err());
    }

    #[test]
    fn sections_map_decode_rejects_invalid_name_length() {
        // Build one descriptor header with name_length = -1.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0i64.to_le_bytes()); // data_size
        bytes.extend_from_slice(&0i64.to_le_bytes()); // max_size
        bytes.extend_from_slice(&0i64.to_le_bytes()); // encrypted
        bytes.extend_from_slice(&0i64.to_le_bytes()); // hashcode
        bytes.extend_from_slice(&(-1i64).to_le_bytes()); // name_length (NEGATIVE)
        bytes.extend_from_slice(&0i64.to_le_bytes()); // unknown
        bytes.extend_from_slice(&0i64.to_le_bytes()); // encoded
        bytes.extend_from_slice(&0i64.to_le_bytes()); // num_pages
        let err = decode_sections_map_content(&bytes, 1);
        assert!(err.is_err());
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
        // Pre-R2007 versions are rejected (different file layout).
        for version in [Version::R12, Version::R14, Version::R2000, Version::R2004] {
            assert!(
                assemble_r2007(empty_parts(version)).is_err(),
                "assemble_r2007 must reject {version:?} (pre-R2007 layout)"
            );
        }
        // R2010/R2013/R2018 also satisfy `uses_utf16_strings()` but
        // are routed through `assemble_r2004` in modern.rs because
        // LibreDWG decodes them via decode_R2004, not decode_R2007.
        // The tightened guard here protects against future callers
        // that bypass modern.rs and reach assemble_r2007 directly
        // with a UTF-16-string version that isn't R2007 — they would
        // otherwise produce a file with the wrong-version signature
        // wrapped around an R2007-style internal layout. This test
        // pins that the guard rejects R2010/R2013/R2018.
        for version in [Version::R2010, Version::R2013, Version::R2018] {
            let err = assemble_r2007(empty_parts(version));
            assert!(
                err.is_err(),
                "assemble_r2007 must reject {version:?} (uses_utf16_strings is true but routing differs)"
            );
        }
        // Sanity check: R2007 itself is accepted.
        assert!(assemble_r2007(empty_parts(Version::R2007)).is_ok());
    }

    #[test]
    fn parse_rejects_non_r2007_version() {
        // Symmetric with `assemble_rejects_non_r2007_version`. We
        // build a real R2007 file (the only kind `assemble_r2007`
        // produces) and then ask `parse_r2007` to interpret it
        // under each non-R2007 version. The version guard must
        // reject every one before any structural decoding starts.
        let bytes = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        for version in [
            Version::R12,
            Version::R14,
            Version::R2000,
            Version::R2004,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            assert!(
                parse_r2007(&bytes, version).is_err(),
                "parse_r2007 must reject {version:?} (not the R2007 codepath)"
            );
        }
        // Sanity check: R2007 itself parses cleanly.
        assert!(parse_r2007(&bytes, Version::R2007).is_ok());
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
        // 7 user pages (sections-map + one data page per mandatory
        // section in `MANDATORY_R2007_SECTION_NAMES`: AcDb:Header,
        // AcDb:AuxHeader, AcDb:Classes, AcDb:Handles, AcDb:Template,
        // AcDb:AcDbObjects) + 1 for the pages-map itself = 8.
        assert_eq!(header.pages_amount, 8);
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

    /// Regression test for the `name_length` consistency invariant
    /// that USED to live as a stored struct field. The fix removed
    /// the field entirely and derives the on-disk byte count from
    /// `name` at encode time, so a caller who mutates `name` after
    /// construction can no longer desynchronize the two.
    ///
    /// Before the fix, the following sequence:
    /// ```ignore
    /// let mut d = SectionDescriptor::empty_for_name("AcDb:Header");
    /// d.name = "x".into();
    /// // d.name_length still says 22, encoded bytes are now 2
    /// ```
    /// would emit a 22-byte name_length but only 2 name bytes,
    /// breaking the decoder. With the field removed, the same
    /// mutation re-encodes consistently.
    #[test]
    fn descriptor_name_mutation_re_encodes_consistently() {
        let mut descriptor = SectionDescriptor::empty_for_name("AcDb:Header");
        descriptor.name = "Z".to_string();
        let mut bytes = Vec::new();
        descriptor.encode(&mut bytes);
        // name_length at offset 32 must reflect the new name
        // (UTF-16LE byte count = 2 for one ASCII code unit).
        assert_eq!(&bytes[32..40], &2i64.to_le_bytes());
        // Total length is 64-byte header + 2-byte name.
        assert_eq!(bytes.len(), 66);
        // And the round-trip through decode preserves the new name.
        let parsed = decode_sections_map_content(&bytes, 1).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "Z");
        assert_eq!(parsed[0].name_length_bytes(), 2);
    }

    /// Regression test for the i64 → usize wrap on a malformed
    /// `pages_map_offset`. Before the explicit `< 0` guard, a
    /// negative offset would cast to a huge usize and *might* slip
    /// past the bounds check in release mode via integer overflow.
    /// Now it must be rejected with a structured error.
    #[test]
    fn parse_rejects_negative_pages_map_offset() {
        // Round-trip a real file, then poke the file-header bytes to
        // overwrite `pages_map_offset` with a negative value.
        use crate::dwg::file::r2007_header::{
            decode_file_header_on_disk, encode_file_header_on_disk, R2007_HEADER_OFFSET,
        };
        let mut file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        let header_region =
            &file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE];
        let mut header = decode_file_header_on_disk(header_region).unwrap();
        header.pages_map_offset = -1;
        let new_region = encode_file_header_on_disk(&header);
        file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
            .copy_from_slice(&new_region);

        let err = parse_r2007(&file, Version::R2007).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("negative pages_map_offset"),
            "expected negative-offset error, got {msg}"
        );
    }

    /// Companion negative-input guards for `pages_map_size_uncomp`
    /// and `pages_map_correction`.
    #[test]
    fn parse_rejects_negative_pages_map_size_uncomp() {
        use crate::dwg::file::r2007_header::{
            decode_file_header_on_disk, encode_file_header_on_disk, R2007_HEADER_OFFSET,
        };
        let mut file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        let header_region =
            &file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE];
        let mut header = decode_file_header_on_disk(header_region).unwrap();
        header.pages_map_size_uncomp = -7;
        let new_region = encode_file_header_on_disk(&header);
        file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
            .copy_from_slice(&new_region);
        let err = parse_r2007(&file, Version::R2007).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("pages_map_size_uncomp"),
            "expected size-uncomp error, got {msg}"
        );
    }

    #[test]
    fn parse_rejects_invalid_pages_map_correction() {
        use crate::dwg::file::r2007_header::{
            decode_file_header_on_disk, encode_file_header_on_disk, R2007_HEADER_OFFSET,
        };
        let mut file = assemble_r2007(empty_parts(Version::R2007)).unwrap();
        let header_region =
            &file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE];
        let mut header = decode_file_header_on_disk(header_region).unwrap();
        header.pages_map_correction = 0;
        let new_region = encode_file_header_on_disk(&header);
        file[R2007_HEADER_OFFSET..R2007_HEADER_OFFSET + R2007_FILE_HEADER_ON_DISK_SIZE]
            .copy_from_slice(&new_region);
        let err = parse_r2007(&file, Version::R2007).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("pages_map_correction"),
            "expected correction error, got {msg}"
        );
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
                "AcDb:Template",
                "AcDb:AcDbObjects",
            ]
        );
    }
}
