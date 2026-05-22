//! Section locator records (R13–R2000) and section identity enum.
//!
//! A section locator is a fixed 9-byte tuple `(id: u8, seeker: u32_le, size: u32_le)`
//! repeated `section_locator_count` times starting at file offset 0x19
//! after the R13–R2000 file header. The locator block is followed by
//! a 2-byte CRC-X25 (seed 0xC0C1) and then the 16-byte `HEADER_END`
//! sentinel.
//!
//! Section IDs match LibreDWG's `Dwg_Section_Type_r13` enum in
//! `include/dwg.h`. Object records (entity / table data) are NOT a
//! distinct section in R13–R2000 — they are written between the
//! Classes section and the Handles map, at file offsets that the
//! Handles map points to.

use crate::dwg::error::{DwgError, DwgResult};

/// Locator entry identifying one section by id and its byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionLocator {
    pub id: SectionId,
    pub seeker: u32,
    pub size: u32,
}

/// Well-known section identifiers used by R13–R2000.
///
/// Numeric values match LibreDWG's `Dwg_Section_Type_r13`:
///   `0=HEADER, 1=CLASSES, 2=HANDLES, 3=OBJFREESPACE, 4=TEMPLATE,
///    5=AUXHEADER, 6=THUMBNAIL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId {
    /// HEADER variables (id=0).
    Header,
    /// CLASSES section (id=1, R13+).
    Classes,
    /// HANDLES — handle → file-offset map. Note: the actual object
    /// records live at the file offsets this map points to; they
    /// are NOT collected in a separate "Objects" section.
    Handles,
    /// OBJFREESPACE (id=3, optional, includes the 2nd-header).
    ObjFreeSpace,
    /// TEMPLATE (id=4, optional, holds MEASUREMENT data since R13c3).
    Template,
    /// AUXHEADER (id=5, R2000 only, no sentinels).
    AuxHeader,
    /// THUMBNAIL (id=6, not a section locator in canonical layout but
    /// emitted in some files).
    Thumbnail,
    /// Unknown or unsupported section id (kept opaque so we can
    /// tolerate sections we don't decode).
    Unknown(u8),
}

impl SectionId {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Header,
            1 => Self::Classes,
            2 => Self::Handles,
            3 => Self::ObjFreeSpace,
            4 => Self::Template,
            5 => Self::AuxHeader,
            6 => Self::Thumbnail,
            other => Self::Unknown(other),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Header => 0,
            Self::Classes => 1,
            Self::Handles => 2,
            Self::ObjFreeSpace => 3,
            Self::Template => 4,
            Self::AuxHeader => 5,
            Self::Thumbnail => 6,
            Self::Unknown(v) => v,
        }
    }
}

/// Parse a contiguous block of `count` section locators starting at
/// `bytes[offset..]`.
pub fn parse_locators(bytes: &[u8], offset: usize, count: u32) -> DwgResult<Vec<SectionLocator>> {
    let needed = (count as usize).saturating_mul(9);
    if bytes.len() < offset.saturating_add(needed) {
        return Err(DwgError::UnexpectedEof {
            byte: offset + needed,
            bit: 0,
        });
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..(count as usize) {
        let p = offset + i * 9;
        let id = SectionId::from_u8(bytes[p]);
        let seeker = u32::from_le_bytes([bytes[p + 1], bytes[p + 2], bytes[p + 3], bytes[p + 4]]);
        let size = u32::from_le_bytes([bytes[p + 5], bytes[p + 6], bytes[p + 7], bytes[p + 8]]);
        out.push(SectionLocator { id, seeker, size });
    }
    Ok(out)
}

/// Encode a list of section locators as their on-disk byte sequence.
pub fn encode_locators(locators: &[SectionLocator]) -> Vec<u8> {
    let mut out = Vec::with_capacity(locators.len() * 9);
    for loc in locators {
        out.push(loc.id.to_u8());
        out.extend_from_slice(&loc.seeker.to_le_bytes());
        out.extend_from_slice(&loc.size.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_id_round_trip() {
        for id in [
            SectionId::Header,
            SectionId::Classes,
            SectionId::Handles,
            SectionId::ObjFreeSpace,
            SectionId::Template,
            SectionId::AuxHeader,
            SectionId::Thumbnail,
            SectionId::Unknown(42),
        ] {
            assert_eq!(SectionId::from_u8(id.to_u8()), id);
        }
    }

    #[test]
    fn locators_round_trip() {
        let want = vec![
            SectionLocator {
                id: SectionId::Header,
                seeker: 0x1000,
                size: 0x200,
            },
            SectionLocator {
                id: SectionId::Classes,
                seeker: 0x1200,
                size: 0x100,
            },
            SectionLocator {
                id: SectionId::Handles,
                seeker: 0x1300,
                size: 0xffff_ffff,
            },
        ];
        let bytes = encode_locators(&want);
        assert_eq!(bytes.len(), 27);
        let got = parse_locators(&bytes, 0, want.len() as u32).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn parse_rejects_truncated_locator_block() {
        // Asking for 2 locators against a 9-byte buffer (only room for 1).
        let bytes = vec![0u8; 9];
        assert!(matches!(
            parse_locators(&bytes, 0, 2),
            Err(DwgError::UnexpectedEof { .. })
        ));
    }
}
