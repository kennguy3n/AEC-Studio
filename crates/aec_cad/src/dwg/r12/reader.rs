//! R12 reader entry point.
//!
//! Walks the file header to locate every section, validates each
//! framed sub-section's CRC, then reconstitutes a
//! [`crate::dxf::DxfDocument`] via the layer/block/index-aware bridge
//! in [`super::bridge`].

use crate::dwg::error::DwgResult;
use crate::dwg::r12::bridge::image_to_document;
use crate::dwg::r12::file::disassemble;
use crate::dwg::r12::R12FileHeader;
use crate::dxf::DxfDocument;

/// Top-level R12 reader.
pub struct R12Reader<'a> {
    pub bytes: &'a [u8],
    pub header: R12FileHeader,
}

impl<'a> R12Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> DwgResult<Self> {
        let header = R12FileHeader::parse(bytes)?;
        Ok(Self { bytes, header })
    }

    /// Convert the in-buffer R12 file into a canonical
    /// [`DxfDocument`]. Disassembles the file into an
    /// [`crate::dwg::r12::bridge::R12FileImage`] (the wire-level
    /// shape) and then maps that to the DXF model.
    pub fn into_document(self) -> DwgResult<DxfDocument> {
        let image = disassemble(self.bytes)?;
        image_to_document(&image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::r12::bridge::document_to_image;
    use crate::dwg::r12::file::assemble;
    use crate::dxf::{DxfDocument, DxfEntity, DxfLine};

    #[test]
    fn empty_document_round_trips_through_reader() {
        let doc = DxfDocument::new();
        let bytes = assemble(&document_to_image(&doc).unwrap()).unwrap();
        let recovered = R12Reader::new(&bytes).unwrap().into_document().unwrap();
        // The default DxfDocument has the dim style "STANDARD"; our
        // writer encodes it as the only DIMSTYLE record, so the round
        // trip preserves it exactly.
        assert_eq!(recovered.entities, doc.entities);
    }

    #[test]
    fn line_document_round_trips_through_reader() {
        let mut doc = DxfDocument::new();
        doc.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [1.0, 2.0, 3.0],
            end: [4.0, 5.0, 6.0],
        }));
        let bytes = assemble(&document_to_image(&doc).unwrap()).unwrap();
        let recovered = R12Reader::new(&bytes).unwrap().into_document().unwrap();
        assert_eq!(recovered.entities, doc.entities);
    }
}
