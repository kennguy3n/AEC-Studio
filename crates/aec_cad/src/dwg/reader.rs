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
            Version::R14 | Version::R2000 => read_modern(self.bytes),
            other => Err(DwgError::UnsupportedInVersion {
                version: other,
                what: format!(
                    "{other:?} decoding requires the R2004+ page system (LZ77 + system \
                     sections + page descriptors), the R2007 UTF-16 string switch, \
                     R2010+ per-version object-table deltas, and (R2018) encrypted \
                     handle pages — these are delivered by subsequent commits in this PR"
                ),
            }),
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
    fn writer_rejects_versions_not_yet_wired() {
        let doc = DxfDocument::new();
        for v in [
            Version::R2004,
            Version::R2007,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            assert!(matches!(
                DwgWriter::write(&doc, v),
                Err(DwgError::UnsupportedInVersion { .. })
            ));
        }
    }
}
