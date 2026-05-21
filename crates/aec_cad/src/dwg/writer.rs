//! Top-level DWG writer — dispatches to the correct version-specific
//! encoder.

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::modern::write_modern;
use crate::dwg::r12::R12Writer;
use crate::dwg::version::Version;
use crate::dxf::DxfDocument;

pub struct DwgWriter;

impl DwgWriter {
    pub fn write(doc: &DxfDocument, version: Version) -> DwgResult<Vec<u8>> {
        match version {
            Version::R12 => R12Writer::write_document(doc),
            Version::R14 | Version::R2000 => write_modern(doc, version),
            other => Err(DwgError::UnsupportedInVersion {
                version: other,
                what: format!(
                    "{other:?} encoding requires the R2004+ page system, R2007 UTF-16 \
                     string switch, R2010+ per-version object-table deltas, and \
                     (R2018) encrypted handle pages — these are delivered by \
                     subsequent commits in this PR"
                ),
            }),
        }
    }
}
