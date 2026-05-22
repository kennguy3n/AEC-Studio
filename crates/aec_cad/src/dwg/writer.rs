//! Top-level DWG writer — dispatches to the correct version-specific
//! encoder.

use crate::dwg::error::DwgResult;
use crate::dwg::modern::write_modern;
use crate::dwg::r12::R12Writer;
use crate::dwg::version::Version;
use crate::dxf::DxfDocument;

pub struct DwgWriter;

impl DwgWriter {
    /// Encode a [`DxfDocument`] to DWG wire bytes for the requested
    /// `version`. Dispatches between:
    /// - R12 (AC1009) — fixed-record codepath under `dwg::r12`
    /// - R14, R2000 — flat section-locator layout
    /// - R2004+ — paged system sections with LZ77-compressed pages
    pub fn write(doc: &DxfDocument, version: Version) -> DwgResult<Vec<u8>> {
        match version {
            Version::R12 => R12Writer::write_document(doc),
            v if v.is_modern() => write_modern(doc, version),
            // is_modern() is exhaustive for everything except R12, so
            // the `v if ..` arm above catches every remaining version.
            // The match is exhaustive even without an `_` fallback.
            #[allow(unreachable_patterns)]
            other => unreachable!("non-modern, non-R12 version slipped past dispatch: {other:?}"),
        }
    }
}
