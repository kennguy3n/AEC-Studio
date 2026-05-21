//! Top-level DWG reader — dispatches to the correct version-specific
//! decoder based on the file signature.

use crate::dwg::error::{DwgError, DwgResult};
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
            other => Err(DwgError::UnsupportedInVersion {
                version: other,
                what: "document decoding is delivered in subsequent commits; the \
                       file-header parser, section locators, entity codecs, and bit \
                       codec are in place — the wiring lands in the next milestone"
                    .into(),
            }),
        }
    }
}
