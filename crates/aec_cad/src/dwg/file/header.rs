//! Modern (R13+) DWG file-header parser/encoder.
//!
//! Field layout (cross-referenced against OpenDesign § "File Header"
//! and LibreDWG `dwg.spec`):
//!
//! ```text
//! offset │ size │ meaning
//! ───────┼──────┼─────────────────────────────────────────
//!   0x00 │   6  │ AC10NN signature (already consumed by version dispatch)
//!   0x06 │   5  │ Five reserved zero bytes
//!   0x0b │   1  │ "Acad maintenance version" byte
//!   0x0c │   1  │ DWG version identifier (echoes AC10NN[5])
//!   0x0d │   4  │ Image preview address (LE u32, 0 if no preview)
//!   0x11 │   1  │ App DWG version
//!   0x12 │   1  │ App maintenance release
//!   0x13 │   2  │ Codepage (LE u16); usually 30 = ANSI_1252
//!   0x15 │   3  │ Padding (zero in modern files)
//!   0x18 │   4  │ Number of section locator records (LE u32)
//!   0x1c │  N×n │ Section locator records (R13–R2000) or system section start (R2004+)
//! ```
//!
//! All numeric fields are little-endian. R2004+ replaces the section
//! locator block with a system-section page header; that's handled in
//! [`super::system_section`].

use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::version::Version;

/// Parsed modern-DWG file header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHeader {
    pub version: Version,
    pub maintenance_release: u8,
    pub preview_offset: u32,
    pub codepage: u16,
    pub section_locator_count: u32,
}

impl FileHeader {
    /// Parse the file header starting at offset 0 of `bytes`. The
    /// 6-byte signature is re-read here for validation against the
    /// expected version (the caller already used it to dispatch).
    pub fn parse(bytes: &[u8], expected: Version) -> DwgResult<Self> {
        if bytes.len() < 0x20 {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        // Verify signature matches what dispatch decided.
        let mut sig = [0u8; 6];
        sig.copy_from_slice(&bytes[..6]);
        if &sig != expected.signature() {
            return Err(DwgError::InvalidSignature(sig));
        }

        let maintenance_release = bytes[0x0b];
        let preview_offset =
            u32::from_le_bytes([bytes[0x0d], bytes[0x0e], bytes[0x0f], bytes[0x10]]);
        let codepage = u16::from_le_bytes([bytes[0x13], bytes[0x14]]);
        let section_locator_count = if expected.has_paged_system_sections() {
            // R2004+ — locator count lives elsewhere; we leave this 0
            // and let the system-section parser populate it.
            0
        } else {
            u32::from_le_bytes([bytes[0x18], bytes[0x19], bytes[0x1a], bytes[0x1b]])
        };

        Ok(Self {
            version: expected,
            maintenance_release,
            preview_offset,
            codepage,
            section_locator_count,
        })
    }

    /// Encode the file header into a fresh `Vec<u8>` of length 0x20.
    /// Section locator records are appended by the caller.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; 0x20];
        buf[..6].copy_from_slice(self.version.signature());
        // 0x06..0x0b: 5 reserved zero bytes (already zero from vec).
        buf[0x0b] = self.maintenance_release;
        buf[0x0c] = self.version.signature()[5]; // echo of the last sig char
        buf[0x0d..0x11].copy_from_slice(&self.preview_offset.to_le_bytes());
        buf[0x11] = 0;
        buf[0x12] = self.maintenance_release;
        buf[0x13..0x15].copy_from_slice(&self.codepage.to_le_bytes());
        // 0x15..0x18: padding.
        buf[0x18..0x1c].copy_from_slice(&self.section_locator_count.to_le_bytes());
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_r2000_header() {
        // Hand-built buffer: AC1015 sig + zeros + section locator count = 5.
        let mut buf = vec![0u8; 0x20];
        buf[..6].copy_from_slice(b"AC1015");
        buf[0x18..0x1c].copy_from_slice(&5u32.to_le_bytes());
        let hdr = FileHeader::parse(&buf, Version::R2000).unwrap();
        assert_eq!(hdr.version, Version::R2000);
        assert_eq!(hdr.section_locator_count, 5);
    }

    #[test]
    fn parse_rejects_mismatched_signature() {
        let mut buf = vec![0u8; 0x20];
        buf[..6].copy_from_slice(b"AC1024");
        assert!(matches!(
            FileHeader::parse(&buf, Version::R2000),
            Err(DwgError::InvalidSignature(_))
        ));
    }

    #[test]
    fn parse_rejects_short_buffer() {
        let buf = vec![0u8; 4];
        assert!(matches!(
            FileHeader::parse(&buf, Version::R2000),
            Err(DwgError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn encode_round_trips() {
        let hdr = FileHeader {
            version: Version::R2010,
            maintenance_release: 4,
            preview_offset: 0xdead_beef,
            codepage: 30,
            section_locator_count: 0, // R2010 is paged
        };
        let bytes = hdr.encode();
        let parsed = FileHeader::parse(&bytes, Version::R2010).unwrap();
        assert_eq!(parsed, hdr);
    }
}
