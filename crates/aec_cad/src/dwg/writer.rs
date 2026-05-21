//! Top-level DWG writer — dispatches to the correct version-specific
//! encoder.

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::R12Writer;
use crate::dwg::version::Version;
use crate::dxf::DxfDocument;

pub struct DwgWriter;

impl DwgWriter {
    pub fn write(doc: &DxfDocument, version: Version) -> DwgResult<Vec<u8>> {
        match version {
            Version::R12 => R12Writer::write_document(doc),
            other => Err(DwgError::UnsupportedInVersion {
                version: other,
                what: "document encoding is delivered in subsequent commits; the \
                       entity codecs and bit codec are in place — the top-level \
                       wiring lands in the next milestone"
                    .into(),
            }),
        }
    }
}
