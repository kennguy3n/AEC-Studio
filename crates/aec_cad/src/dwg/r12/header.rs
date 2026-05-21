//! R12 (AC1009) file header.

use crate::dwg::error::{DwgError, DwgResult};

/// Offsets to the various R12 table sections inside the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct R12FileHeader {
    pub preview_offset: u32,
    pub block_offset: u32,
    pub layer_offset: u32,
    pub linetype_offset: u32,
    pub style_offset: u32,
    pub view_offset: u32,
    pub ucs_offset: u32,
    pub viewport_offset: u32,
    pub dimstyle_offset: u32,
    pub appid_offset: u32,
    pub entities_offset: u32,
    pub eof_offset: u32,
}

impl R12FileHeader {
    pub fn parse(bytes: &[u8]) -> DwgResult<Self> {
        if bytes.len() < 0x3c {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        if &bytes[..6] != b"AC1009" {
            let mut sig = [0u8; 6];
            sig.copy_from_slice(&bytes[..6]);
            return Err(DwgError::InvalidSignature(sig));
        }
        fn read_u32(b: &[u8], at: usize) -> u32 {
            u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
        }
        Ok(Self {
            preview_offset: read_u32(bytes, 0x0c),
            block_offset: read_u32(bytes, 0x10),
            layer_offset: read_u32(bytes, 0x14),
            linetype_offset: read_u32(bytes, 0x18),
            style_offset: read_u32(bytes, 0x1c),
            view_offset: read_u32(bytes, 0x20),
            ucs_offset: read_u32(bytes, 0x24),
            viewport_offset: read_u32(bytes, 0x28),
            dimstyle_offset: read_u32(bytes, 0x2c),
            appid_offset: read_u32(bytes, 0x30),
            entities_offset: read_u32(bytes, 0x34),
            eof_offset: read_u32(bytes, 0x38),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; 0x3c];
        buf[..6].copy_from_slice(b"AC1009");
        // 0x06..0x0c: zero padding.
        let writes = [
            (0x0c, self.preview_offset),
            (0x10, self.block_offset),
            (0x14, self.layer_offset),
            (0x18, self.linetype_offset),
            (0x1c, self.style_offset),
            (0x20, self.view_offset),
            (0x24, self.ucs_offset),
            (0x28, self.viewport_offset),
            (0x2c, self.dimstyle_offset),
            (0x30, self.appid_offset),
            (0x34, self.entities_offset),
            (0x38, self.eof_offset),
        ];
        for (off, val) in writes {
            buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r12_header_round_trips() {
        let want = R12FileHeader {
            preview_offset: 0x100,
            block_offset: 0x200,
            layer_offset: 0x300,
            linetype_offset: 0x400,
            style_offset: 0x500,
            view_offset: 0x600,
            ucs_offset: 0x700,
            viewport_offset: 0x800,
            dimstyle_offset: 0x900,
            appid_offset: 0xa00,
            entities_offset: 0xb00,
            eof_offset: 0xc00,
        };
        let bytes = want.encode();
        let got = R12FileHeader::parse(&bytes).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn r12_header_rejects_wrong_signature() {
        let mut buf = vec![0u8; 0x3c];
        buf[..6].copy_from_slice(b"AC1014");
        assert!(matches!(
            R12FileHeader::parse(&buf),
            Err(DwgError::InvalidSignature(_))
        ));
    }

    #[test]
    fn r12_header_rejects_short_buffer() {
        let buf = vec![0u8; 0x20];
        assert!(matches!(
            R12FileHeader::parse(&buf),
            Err(DwgError::UnexpectedEof { .. })
        ));
    }
}
