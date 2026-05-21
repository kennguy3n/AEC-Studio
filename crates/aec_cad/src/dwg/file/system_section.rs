//! R2004+ system section + 2nd-header layout.
//!
//! Starting with R2004, AutoCAD replaced the flat section-locator
//! block with a "system section" that contains paged data. Each page
//! is independently compressed (LZ77-style) and CRC-32C-checked.
//! Pages are addressed via a page map (page id → file offset) and a
//! section map (logical section id → page id list).
//!
//! This module currently provides the structural layer (section /
//! page descriptors and their byte layout). The actual page paging
//! and compression are wired in alongside R2004+ entity codecs.

use crate::dwg::error::{DwgError, DwgResult};

/// A single page descriptor inside the R2004+ page map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageDescriptor {
    pub page_id: u32,
    pub page_size: u32,
    pub file_offset: u64,
}

/// A logical section made up of one or more pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionDescriptor {
    pub section_id: u64,
    pub name: String,
    pub max_decompressed_size: u64,
    pub encoding: SectionEncoding,
    pub pages: Vec<u32>,
}

/// How a section's pages are stored on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEncoding {
    /// Raw (uncompressed) pages.
    Raw,
    /// LZ77-style compressed pages (R2004+).
    Compressed,
    /// Encrypted with the magic-byte XOR mask (R2018 handle pages).
    Encrypted,
}

/// Parse the section/page descriptors. The R2004+ system-section
/// layout is large and version-sensitive; we currently expose the
/// shape so callers can build it programmatically in tests and so
/// future version dispatch has a single place to land.
pub fn parse_descriptors(
    _bytes: &[u8],
) -> DwgResult<(Vec<PageDescriptor>, Vec<SectionDescriptor>)> {
    // Implementing the R2004+ page/section parser is a substantial
    // chunk of dense bit math that must match AutoCAD byte-for-byte.
    // It lands alongside the R2004+ reader integration; until then
    // callers attempting to read R2004+ files get a structured error
    // from [`crate::dwg::reader::Reader::from_bytes`] rather than a
    // silent fallthrough.
    Err(DwgError::InternalInvariant(
        "R2004+ system-section parser not yet wired in this branch — caller \
         should dispatch to a version-specific path before reaching here"
            .to_string(),
    ))
}
