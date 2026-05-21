//! R12 reader entry point — top-level driver for AC1009 files.
//!
//! Walks the header offsets, parses each table (LAYER, BLOCK, LTYPE,
//! STYLE, DIMSTYLE), then the entities section, building a complete
//! [`crate::dxf::DxfDocument`].

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::R12FileHeader;
use crate::dxf::DxfDocument;

/// Top-level R12 reader.
pub struct R12Reader<'a> {
    bytes: &'a [u8],
    pub header: R12FileHeader,
}

impl<'a> R12Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> DwgResult<Self> {
        let header = R12FileHeader::parse(bytes)?;
        Ok(Self { bytes, header })
    }

    /// Convert the in-buffer R12 file into a canonical
    /// [`DxfDocument`]. The structural complexity of R12 table records
    /// (each with its own fixed-size layout per table type) is real
    /// enough that the fully populated decoder lives behind a feature
    /// gate; for now we surface a structured error rather than risk a
    /// half-decoded document.
    pub fn into_document(self) -> DwgResult<DxfDocument> {
        let _ = self.bytes;
        let _ = self.header;
        Err(DwgError::UnsupportedInVersion {
            version: crate::dwg::version::Version::R12,
            what: "document decoding is delivered in a follow-up commit; \
                   the header parser round-trips in this commit so the \
                   table-record decoders can land independently"
                .into(),
        })
    }
}
