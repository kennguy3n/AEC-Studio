//! R12 writer entry point.
//!
//! Builds an [`crate::dwg::r12::bridge::R12FileImage`] from a
//! [`crate::dxf::DxfDocument`] and asks
//! [`crate::dwg::r12::file::assemble`] to serialise it.

use crate::dwg::error::DwgResult;
use crate::dwg::r12::bridge::document_to_image;
use crate::dwg::r12::file::assemble;
use crate::dxf::DxfDocument;

pub struct R12Writer;

impl R12Writer {
    /// Serialise `doc` as an AC1009 file image. The output is
    /// self-describing — the leading 60 bytes are the AC1009 file
    /// header with per-section locator offsets, every section ends
    /// in a CRC-X25, and the trailing EOF offset matches the byte
    /// length of the output.
    pub fn write_document(doc: &DxfDocument) -> DwgResult<Vec<u8>> {
        let image = document_to_image(doc)?;
        assemble(&image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::r12::bridge::image_to_document;
    use crate::dwg::r12::file::disassemble;
    use crate::dxf::{DxfArc, DxfCircle, DxfDocument, DxfEntity, DxfLine};

    #[test]
    fn full_round_trip_via_writer_and_reader() {
        let mut doc = DxfDocument::new();
        doc.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".to_string(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 0.0, 0.0],
        }));
        doc.entities.push(DxfEntity::Circle(DxfCircle {
            layer: "0".to_string(),
            center: [5.0, 5.0, 0.0],
            radius: 2.5,
        }));
        doc.entities.push(DxfEntity::Arc(DxfArc {
            layer: "0".to_string(),
            center: [10.0, 10.0, 0.0],
            radius: 1.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        }));
        let bytes = R12Writer::write_document(&doc).unwrap();
        let recovered = image_to_document(&disassemble(&bytes).unwrap()).unwrap();
        assert_eq!(recovered.entities, doc.entities);
    }
}
