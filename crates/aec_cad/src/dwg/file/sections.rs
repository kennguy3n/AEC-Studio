//! Section locator records (R13–R2000) and section identity enum.
//!
//! A section locator is a fixed 9-byte tuple `(id: u8, seeker: u32_le, size: u32_le)`
//! repeated `section_locator_count` times starting at file offset 0x1c
//! after a R13–R2000 file header. The locator block is followed by
//! a section CRC.

use crate::dwg::error::{DwgError, DwgResult};

/// Locator entry identifying one section by id and its byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionLocator {
    pub id: SectionId,
    pub seeker: u32,
    pub size: u32,
}

/// Well-known section identifiers used by R13–R2000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId {
    /// HEADER variables.
    Header,
    /// CLASSES section (R13+).
    Classes,
    /// OBJECTS — the entity and table data.
    Objects,
    /// OBJECT_MAP — handle → file-offset map.
    ObjectMap,
    /// Unknown or unsupported section id (kept opaque so we can
    /// tolerate sections we don't decode).
    Unknown(u8),
}

impl SectionId {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Header,
            1 => Self::Classes,
            2 => Self::Objects,
            3 => Self::ObjectMap,
            other => Self::Unknown(other),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Header => 0,
            Self::Classes => 1,
            Self::Objects => 2,
            Self::ObjectMap => 3,
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
            SectionId::Objects,
            SectionId::ObjectMap,
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
                id: SectionId::Objects,
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
