//! R12 writer entry point.
//!
//! Walks a [`crate::dxf::DxfDocument`] and emits an AC1009 file image.

use crate::dwg::error::{DwgError, DwgResult};
use crate::dxf::DxfDocument;

pub struct R12Writer;

impl R12Writer {
    pub fn write_document(_doc: &DxfDocument) -> DwgResult<Vec<u8>> {
        Err(DwgError::UnsupportedInVersion {
            version: crate::dwg::version::Version::R12,
            what: "document encoding is delivered in a follow-up commit; the \
                   header writer in this commit pins the format so the table-record \
                   encoders can land independently"
                .into(),
        })
    }
}
