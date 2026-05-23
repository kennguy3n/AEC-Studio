//! AC1009 (AutoCAD R11/R12) file header: bytes `0x00..0x2C`.
//!
//! The layout below is taken verbatim from LibreDWG's `header.spec`
//! (the `VERSIONS(R_2_0b, R_13b1)` block at lines 24-48). The total
//! width is 44 bytes; the per-section table headers start at `0x2C`.
//!
//! ```text
//! 0x00  version[11]            "AC1009\0\0\0\0\0" — 11 ASCII bytes
//! 0x0B  RC maint_rel_version   1 byte
//! 0x0C  RC zero_one_or_three   1 byte (R11/R12: 0)
//! 0x0D  RS numentity_sections  2 bytes LE
//! 0x0F  RS sections            2 bytes LE (cast to RL on read) =
//!                              `num_sections` (0..SECTION_VX)
//! 0x11  RS numheader_vars      2 bytes LE — 204 for AC1009
//! 0x13  RC dwg_version         1 byte (R11: 0)
//! 0x14  RLx entities_start     4 bytes LE
//! 0x18  RLx entities_end       4 bytes LE
//! 0x1C  RLx blocks_start       4 bytes LE
//! 0x20  RLx blocks_size        4 bytes LE
//! 0x24  RLx extras_start       4 bytes LE
//! 0x28  RLx extras_size        4 bytes LE
//! 0x2C  (per-section table headers begin)
//! ```
//!
//! All fields are little-endian. The six `RLx` offsets at `0x14..0x2C`
//! are the section-locator block — LibreDWG patches these in last
//! (`encode.c:3151-3174`), so we likewise leave them as placeholders
//! during the initial write and patch them in once the trailing
//! sections have been laid out.

use crate::dwg::error::{DwgError, DwgResult};

/// Total width of the file header in bytes. The per-section table
/// headers begin immediately after.
pub const FILE_HEADER_LEN: usize = 0x2C;

/// The 11-byte ASCII version prefix for R12 files.
///
/// Padded with NULs to fill the 11-byte slot exactly. LibreDWG only
/// matches the first 6 bytes (`AC1009`), but real AutoCAD-emitted
/// files include the NUL padding, and shipping the same padding keeps
/// the file byte-identical to what AutoDesk's tools produce.
pub const R12_VERSION_STRING: [u8; 11] = *b"AC1009\0\0\0\0\0";

/// Section locator block — the six `RLx` fields at `0x14..0x2C` of
/// the file header. Owned separately because it is patched in after
/// every other section has been laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct R12SectionLocator {
    /// First byte of the `DWG_SENTINEL_R11_ENTITIES_BEGIN` sentinel +
    /// 16 (i.e., the first byte of entity payload). LibreDWG reads
    /// the BEGIN sentinel at `entities_start - 16` and the per-file
    /// CRC at `entities_start - 18`.
    pub entities_start: u32,
    /// One-past-the-last byte of entity payload (before the
    /// `DWG_SENTINEL_R11_ENTITIES_END` sentinel).
    pub entities_end: u32,
    /// First byte of block-entity payload (after the
    /// `DWG_SENTINEL_R11_BLOCK_ENTITIES_BEGIN` sentinel).
    pub blocks_start: u32,
    /// Byte-length of the block-entity payload. Encoded with the
    /// `0x40000000` flag added when `version > R_2_22` (always true
    /// for R12). The decoder strips the high bits via `& 0xffffff`.
    pub blocks_size: u32,
    /// First byte of extra-entity (objects) payload.
    pub extras_start: u32,
    /// Byte-length of the extra-entity payload with the `0x80000000`
    /// flag added (R12+).
    pub extras_size: u32,
}

/// Encoded form of the 44-byte file header. The byte buffer is
/// initialised with the static fields and zero-placeholders for the
/// six locator offsets; back-patch using [`patch_section_locator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct R12FileHeader {
    pub maint_rel_version: u8,
    pub zero_one_or_three: u8,
    pub numentity_sections: u16,
    pub num_sections: u16,
    pub numheader_vars: u16,
    pub dwg_version: u8,
    pub locator: R12SectionLocator,
}

impl Default for R12FileHeader {
    /// Defaults match what LibreDWG emits for a fresh AC1009 file.
    ///
    /// * `numentity_sections = 3` — LibreDWG always writes 3 here for
    ///   pre-R13 files (see `header.spec:36` and the `Dwg_Header`
    ///   default in `dwg.spec`).
    /// * `num_sections = 11` — `SECTION_VX`, enabling all 10 table
    ///   sections (BLOCK..VX). LibreDWG defaults this for R11+ at
    ///   `encode.c:2991`.
    /// * `numheader_vars = 205` — AC1009's documented value (the
    ///   "AC1009 r11" emission per the comment block at
    ///   `header.spec:34`). LibreDWG accepts any value `<= 205`;
    ///   205 emits the full var set.
    fn default() -> Self {
        Self {
            maint_rel_version: 0,
            zero_one_or_three: 0,
            numentity_sections: 3,
            num_sections: 11, // SECTION_VX
            numheader_vars: 205,
            dwg_version: 0,
            locator: R12SectionLocator::default(),
        }
    }
}

impl R12FileHeader {
    /// Emit the 44-byte file header. The locator offsets are written
    /// as-is; callers that don't yet know them should leave
    /// `Self::locator` zero and call [`patch_section_locator`] later.
    pub fn encode(&self) -> [u8; FILE_HEADER_LEN] {
        let mut buf = [0u8; FILE_HEADER_LEN];
        buf[0x00..0x0B].copy_from_slice(&R12_VERSION_STRING);
        buf[0x0B] = self.maint_rel_version;
        buf[0x0C] = self.zero_one_or_three;
        buf[0x0D..0x0F].copy_from_slice(&self.numentity_sections.to_le_bytes());
        buf[0x0F..0x11].copy_from_slice(&self.num_sections.to_le_bytes());
        buf[0x11..0x13].copy_from_slice(&self.numheader_vars.to_le_bytes());
        buf[0x13] = self.dwg_version;
        buf[0x14..0x18].copy_from_slice(&self.locator.entities_start.to_le_bytes());
        buf[0x18..0x1C].copy_from_slice(&self.locator.entities_end.to_le_bytes());
        buf[0x1C..0x20].copy_from_slice(&self.locator.blocks_start.to_le_bytes());
        buf[0x20..0x24].copy_from_slice(&self.locator.blocks_size.to_le_bytes());
        buf[0x24..0x28].copy_from_slice(&self.locator.extras_start.to_le_bytes());
        buf[0x28..0x2C].copy_from_slice(&self.locator.extras_size.to_le_bytes());
        buf
    }

    /// Parse the 44-byte file header.
    pub fn parse(bytes: &[u8]) -> DwgResult<Self> {
        if bytes.len() < FILE_HEADER_LEN {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        if bytes[..6] != R12_VERSION_STRING[..6] {
            let mut sig = [0u8; 6];
            sig.copy_from_slice(&bytes[..6]);
            return Err(DwgError::InvalidSignature(sig));
        }
        Ok(Self {
            maint_rel_version: bytes[0x0B],
            zero_one_or_three: bytes[0x0C],
            numentity_sections: u16::from_le_bytes([bytes[0x0D], bytes[0x0E]]),
            num_sections: u16::from_le_bytes([bytes[0x0F], bytes[0x10]]),
            numheader_vars: u16::from_le_bytes([bytes[0x11], bytes[0x12]]),
            dwg_version: bytes[0x13],
            locator: R12SectionLocator {
                entities_start: u32::from_le_bytes([
                    bytes[0x14],
                    bytes[0x15],
                    bytes[0x16],
                    bytes[0x17],
                ]),
                entities_end: u32::from_le_bytes([
                    bytes[0x18],
                    bytes[0x19],
                    bytes[0x1A],
                    bytes[0x1B],
                ]),
                blocks_start: u32::from_le_bytes([
                    bytes[0x1C],
                    bytes[0x1D],
                    bytes[0x1E],
                    bytes[0x1F],
                ]),
                blocks_size: u32::from_le_bytes([
                    bytes[0x20],
                    bytes[0x21],
                    bytes[0x22],
                    bytes[0x23],
                ]),
                extras_start: u32::from_le_bytes([
                    bytes[0x24],
                    bytes[0x25],
                    bytes[0x26],
                    bytes[0x27],
                ]),
                extras_size: u32::from_le_bytes([
                    bytes[0x28],
                    bytes[0x29],
                    bytes[0x2A],
                    bytes[0x2B],
                ]),
            },
        })
    }
}

/// Patch the six section-locator fields at offset `0x14..0x2C` of an
/// already-encoded file. Used after the rest of the file has been laid
/// out and the offsets are known.
pub fn patch_section_locator(buf: &mut [u8], locator: R12SectionLocator) -> DwgResult<()> {
    if buf.len() < FILE_HEADER_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: buf.len(),
            bit: 0,
        });
    }
    buf[0x14..0x18].copy_from_slice(&locator.entities_start.to_le_bytes());
    buf[0x18..0x1C].copy_from_slice(&locator.entities_end.to_le_bytes());
    buf[0x1C..0x20].copy_from_slice(&locator.blocks_start.to_le_bytes());
    buf[0x20..0x24].copy_from_slice(&locator.blocks_size.to_le_bytes());
    buf[0x24..0x28].copy_from_slice(&locator.extras_start.to_le_bytes());
    buf[0x28..0x2C].copy_from_slice(&locator.extras_size.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_header_round_trips() {
        let want = R12FileHeader::default();
        let bytes = want.encode();
        let got = R12FileHeader::parse(&bytes).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn header_round_trips_with_real_locator() {
        let want = R12FileHeader {
            locator: R12SectionLocator {
                entities_start: 0x1234,
                entities_end: 0x5678,
                blocks_start: 0x9ABC,
                blocks_size: 0x40000010,
                extras_start: 0xDEAD,
                extras_size: 0x80000020,
            },
            ..Default::default()
        };
        let bytes = want.encode();
        let got = R12FileHeader::parse(&bytes).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn header_rejects_wrong_signature() {
        let mut buf = [0u8; FILE_HEADER_LEN];
        buf[..6].copy_from_slice(b"AC1014");
        let err = R12FileHeader::parse(&buf).unwrap_err();
        assert!(matches!(err, DwgError::InvalidSignature(_)));
    }

    #[test]
    fn header_rejects_short_buffer() {
        let buf = [0u8; 16];
        let err = R12FileHeader::parse(&buf).unwrap_err();
        assert!(matches!(err, DwgError::UnexpectedEof { .. }));
    }

    #[test]
    fn patch_section_locator_overwrites_in_place() {
        let mut buf = R12FileHeader::default().encode().to_vec();
        // Initial locator is all-zero.
        let header = R12FileHeader::parse(&buf).unwrap();
        assert_eq!(header.locator, R12SectionLocator::default());

        let patched = R12SectionLocator {
            entities_start: 0xA0,
            entities_end: 0xC0,
            blocks_start: 0xE0,
            blocks_size: 0x40000000,
            extras_start: 0x100,
            extras_size: 0x80000000,
        };
        patch_section_locator(&mut buf, patched).unwrap();
        let header = R12FileHeader::parse(&buf).unwrap();
        assert_eq!(header.locator, patched);
    }

    #[test]
    fn file_header_starts_at_zero() {
        // FILE_HEADER_LEN must equal 0x2C — section table headers
        // begin at exactly this offset.
        assert_eq!(FILE_HEADER_LEN, 0x2C);
    }

    #[test]
    fn version_string_matches_libredwg_pattern() {
        assert_eq!(&R12_VERSION_STRING[..6], b"AC1009");
        assert_eq!(&R12_VERSION_STRING[6..], &[0u8; 5]);
    }
}
