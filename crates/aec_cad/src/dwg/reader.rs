//! Top-level DWG reader — dispatches to the correct version-specific
//! decoder based on the file signature.

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::modern::read_modern;
use crate::dwg::r12::R12Reader;
use crate::dwg::version::{detect, Version};
use crate::dxf::DxfDocument;

/// High-level entry: take a byte buffer, return a canonical
/// [`DxfDocument`]. Version dispatch happens here.
pub struct DwgReader<'a> {
    bytes: &'a [u8],
    pub version: Version,
}

impl<'a> DwgReader<'a> {
    pub fn new(bytes: &'a [u8]) -> DwgResult<Self> {
        let version = detect(bytes).ok_or_else(|| {
            let mut sig = [0u8; 6];
            let n = bytes.len().min(6);
            sig[..n].copy_from_slice(&bytes[..n]);
            DwgError::InvalidSignature(sig)
        })?;
        Ok(Self { bytes, version })
    }

    pub fn into_document(self) -> DwgResult<DxfDocument> {
        match self.version {
            Version::R12 => R12Reader::new(self.bytes)?.into_document(),
            v if v.is_modern() => read_modern(self.bytes),
            // is_modern() covers R14 and everything above; R12 is
            // handled by the explicit arm above. The match is
            // exhaustive — this arm is unreachable.
            #[allow(unreachable_patterns)]
            other => unreachable!("non-modern, non-R12 version slipped past dispatch: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::writer::DwgWriter;
    use crate::dxf::{DxfEntity, DxfLine};

    fn one_line_document() -> DxfDocument {
        let mut doc = DxfDocument::new();
        doc.entities.push(DxfEntity::Line(DxfLine {
            layer: "0".into(),
            start: [0.0, 0.0, 0.0],
            end: [10.0, 5.0, 0.0],
        }));
        doc
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2000() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2000).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2000);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Line(l) => {
                assert_eq!(l.start, [0.0, 0.0, 0.0]);
                assert_eq!(l.end, [10.0, 5.0, 0.0]);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r12() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R12).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R12);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Line(l) => {
                assert_eq!(l.start, [0.0, 0.0, 0.0]);
                assert_eq!(l.end, [10.0, 5.0, 0.0]);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r14() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R14).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R14);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2004() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2004).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2004);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
        match &back.entities[0] {
            DxfEntity::Line(l) => {
                assert_eq!(l.start, [0.0, 0.0, 0.0]);
                assert_eq!(l.end, [10.0, 5.0, 0.0]);
            }
            other => panic!("expected Line, got {other:?}"),
        }
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2007() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2007).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2007);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2010() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2010).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2010);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2013() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2013).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2013);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
    }

    #[test]
    fn line_round_trips_through_dwg_writer_reader_r2018() {
        let doc = one_line_document();
        let bytes = DwgWriter::write(&doc, Version::R2018).unwrap();
        let reader = DwgReader::new(&bytes).unwrap();
        assert_eq!(reader.version, Version::R2018);
        let back = reader.into_document().unwrap();
        assert_eq!(back.entities.len(), 1);
    }
}
