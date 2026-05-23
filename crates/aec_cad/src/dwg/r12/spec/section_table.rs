//! Per-section table headers ("section descriptors") for AC1009.
//!
//! Each descriptor is 10 bytes on disk:
//!
//! ```text
//! offset │ width │ field
//! ───────┼───────┼─────────────────────────────────────
//!     0  │  2 RS │ size       — sizeof one record (>= 33 when number > 0)
//!     2  │  2 RS │ number     — number of records in the table
//!     4  │  2 RS │ flags_r11  — table-wide flags
//!     6  │  4 RL │ address    — absolute file offset of the table's
//!                              first record (0 when number == 0)
//! ```
//!
//! For R12, the ten section descriptors are split across two
//! locations in the file:
//!
//! 1. **The contiguous section-table region at `0x2C..0x5E`** holds
//!    exactly five descriptors, in this order (per
//!    `decode_r11.c:683-695` and `encode.c:2977-2981`):
//!    * BLOCK    (SECTION_BLOCK = 1)
//!    * LAYER    (SECTION_LAYER = 2)
//!    * STYLE    (SECTION_STYLE = 3)
//!    * LTYPE    (SECTION_LTYPE = 5)
//!    * VIEW     (SECTION_VIEW  = 6)
//!
//! 2. **Five further descriptors are interleaved inside the
//!    header-variables block** at fixed offsets (per
//!    `header_variables_r11.spec:269, 326, 333, 339, 376`):
//!    * UCS      (after FIELD_CAST(MIRRTEXT, …))
//!    * VPORT    (after FIELD_RS(SURFTAB2, …))
//!    * APPID    (after FIELD_HANDLE(UCSNAME, …))
//!    * DIMSTYLE (after FIELD_RS(unknown_520, …))
//!    * VX       (after FIELD_3RD(PINSBASE, …))
//!
//! The split layout is a historical quirk of AutoDesk's R11 spec;
//! LibreDWG's `PRER13_SECTION_HDR` macro emits the descriptor
//! wherever its invocation appears in the spec source. Our encoder
//! mirrors this — five descriptors are written by [`R12SectionTables::
//! encode_leading_five_into`] and the other five are written by
//! `header_vars::encode` at the right offsets.
//!
//! LibreDWG enforces these structural invariants on read:
//! * If `number > 0`, then `size >= 33` (`decode_r11.c:210-214`,
//!   error: "Wrong %s.number ... or size %u").
//! * If `number > 0`, the table footprint must fit in the file:
//!   `address + number * size <= dat->size` (`decode_r11.c:216-223`).
//!
//! The empty-document encoder leaves every descriptor at
//! `number = 0` and `address = 0`, trivially satisfying both
//! invariants.

use crate::dwg::error::{DwgError, DwgResult};

/// Width of one section table header on disk.
pub const SECTION_TABLE_HEADER_LEN: usize = 10;

/// Number of section descriptors emitted contiguously at `0x2C..0x5E`.
pub const LEADING_SECTION_COUNT: usize = 5;

/// Total byte width of the contiguous region at `0x2C..0x5E`.
pub const LEADING_SECTION_REGION_LEN: usize = SECTION_TABLE_HEADER_LEN * LEADING_SECTION_COUNT;

/// File offset at which the first section descriptor (BLOCK) begins.
pub const LEADING_SECTION_REGION_START: usize = 0x2C;

/// File offset at which the contiguous section-table region ends
/// (= start of header_vars).
pub const LEADING_SECTION_REGION_END: usize =
    LEADING_SECTION_REGION_START + LEADING_SECTION_REGION_LEN;

/// One section's descriptor — `(size, number, flags_r11, address)`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SectionTableHeader {
    /// Per-record size in bytes. Must be `>= 33` when `number > 0`,
    /// matching LibreDWG's invariant at `decode_r11.c:210-214`. Zero
    /// is permitted only when `number == 0`.
    pub size: u16,
    /// Number of records in this table.
    pub number: u16,
    /// Table-wide flags. Computed by `calc_preR13_ctrl_flags_r11`
    /// in LibreDWG; for the empty-document path we leave this at 0.
    pub flags_r11: u16,
    /// Absolute file offset of the first record. Zero when the table
    /// is empty.
    pub address: u32,
}

impl SectionTableHeader {
    /// Validate the descriptor's structural invariants. Returns
    /// `Err` with a structured `DwgError` if `(number, size)` would
    /// trip the corresponding LibreDWG check.
    pub fn validate(&self, name: &'static str) -> DwgResult<()> {
        if self.number > 0 && self.size < 33 {
            return Err(DwgError::InternalInvariant(format!(
                "{} section descriptor has number={} but size={} (<33); \
                 LibreDWG rejects this in decode_preR13_section_hdr",
                name, self.number, self.size
            )));
        }
        Ok(())
    }

    /// Encode the 10-byte descriptor into `out[..10]`. Panics if the
    /// slice is shorter than 10 bytes.
    pub fn encode_into(&self, out: &mut [u8]) {
        assert!(
            out.len() >= SECTION_TABLE_HEADER_LEN,
            "section table header needs 10 bytes, got {}",
            out.len()
        );
        out[0..2].copy_from_slice(&self.size.to_le_bytes());
        out[2..4].copy_from_slice(&self.number.to_le_bytes());
        out[4..6].copy_from_slice(&self.flags_r11.to_le_bytes());
        out[6..10].copy_from_slice(&self.address.to_le_bytes());
    }

    /// Append the 10-byte descriptor to `out`.
    pub fn encode_append(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.resize(start + SECTION_TABLE_HEADER_LEN, 0);
        self.encode_into(&mut out[start..]);
    }

    /// Parse a 10-byte descriptor from `bytes[..10]`.
    pub fn parse(bytes: &[u8]) -> DwgResult<Self> {
        if bytes.len() < SECTION_TABLE_HEADER_LEN {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        Ok(Self {
            size: u16::from_le_bytes([bytes[0], bytes[1]]),
            number: u16::from_le_bytes([bytes[2], bytes[3]]),
            flags_r11: u16::from_le_bytes([bytes[4], bytes[5]]),
            address: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
        })
    }
}

/// All ten R12 section descriptors held together. The encoder emits
/// the leading five at the section-table region and the trailing five
/// at their (header_vars-internal) positions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct R12SectionTables {
    pub block: SectionTableHeader,
    pub layer: SectionTableHeader,
    pub style: SectionTableHeader,
    pub ltype: SectionTableHeader,
    pub view: SectionTableHeader,
    pub ucs: SectionTableHeader,
    pub vport: SectionTableHeader,
    pub appid: SectionTableHeader,
    pub dimstyle: SectionTableHeader,
    pub vx: SectionTableHeader,
}

impl R12SectionTables {
    /// Encode the five contiguous descriptors at `0x2C..0x5E`.
    pub fn encode_leading_five(&self) -> [u8; LEADING_SECTION_REGION_LEN] {
        let mut buf = [0u8; LEADING_SECTION_REGION_LEN];
        let order = [
            &self.block,
            &self.layer,
            &self.style,
            &self.ltype,
            &self.view,
        ];
        for (i, d) in order.iter().enumerate() {
            let off = i * SECTION_TABLE_HEADER_LEN;
            d.encode_into(&mut buf[off..off + SECTION_TABLE_HEADER_LEN]);
        }
        buf
    }

    /// Parse the five contiguous descriptors at `0x2C..0x5E`.
    pub fn parse_leading_five(
        bytes: &[u8],
    ) -> DwgResult<[SectionTableHeader; LEADING_SECTION_COUNT]> {
        if bytes.len() < LEADING_SECTION_REGION_LEN {
            return Err(DwgError::UnexpectedEof {
                byte: bytes.len(),
                bit: 0,
            });
        }
        let mut out = [SectionTableHeader::default(); LEADING_SECTION_COUNT];
        for (i, slot) in out.iter_mut().enumerate() {
            let off = i * SECTION_TABLE_HEADER_LEN;
            *slot = SectionTableHeader::parse(&bytes[off..off + SECTION_TABLE_HEADER_LEN])?;
        }
        Ok(out)
    }

    /// Validate every descriptor's structural invariants.
    pub fn validate_all(&self) -> DwgResult<()> {
        let pairs: [(&SectionTableHeader, &'static str); 10] = [
            (&self.block, "BLOCK"),
            (&self.layer, "LAYER"),
            (&self.style, "STYLE"),
            (&self.ltype, "LTYPE"),
            (&self.view, "VIEW"),
            (&self.ucs, "UCS"),
            (&self.vport, "VPORT"),
            (&self.appid, "APPID"),
            (&self.dimstyle, "DIMSTYLE"),
            (&self.vx, "VX"),
        ];
        for (d, name) in pairs.iter() {
            d.validate(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_descriptor_round_trips() {
        let want = SectionTableHeader::default();
        let mut buf = [0u8; SECTION_TABLE_HEADER_LEN];
        want.encode_into(&mut buf);
        let got = SectionTableHeader::parse(&buf).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn populated_descriptor_round_trips() {
        let want = SectionTableHeader {
            size: 51,
            number: 2,
            flags_r11: 0x42,
            address: 0xDEADBEEF,
        };
        let mut buf = [0u8; SECTION_TABLE_HEADER_LEN];
        want.encode_into(&mut buf);
        let got = SectionTableHeader::parse(&buf).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn descriptor_validate_rejects_undersized_record_for_nonempty_table() {
        let bad = SectionTableHeader {
            size: 16,
            number: 3,
            ..Default::default()
        };
        let err = bad.validate("LAYER").unwrap_err();
        assert!(matches!(err, DwgError::InternalInvariant(_)));
    }

    #[test]
    fn descriptor_validate_allows_zero_size_when_number_zero() {
        let ok = SectionTableHeader::default();
        assert!(ok.validate("BLOCK").is_ok());
    }

    #[test]
    fn leading_five_round_trips() {
        let tables = R12SectionTables {
            block: SectionTableHeader {
                size: 51,
                number: 1,
                flags_r11: 0,
                address: 0x1000,
            },
            view: SectionTableHeader {
                size: 58,
                number: 2,
                flags_r11: 0x80,
                address: 0x2000,
            },
            ..Default::default()
        };
        let bytes = tables.encode_leading_five();
        let parsed = R12SectionTables::parse_leading_five(&bytes).unwrap();
        assert_eq!(parsed[0], tables.block);
        assert_eq!(parsed[1], tables.layer);
        assert_eq!(parsed[2], tables.style);
        assert_eq!(parsed[3], tables.ltype);
        assert_eq!(parsed[4], tables.view);
    }

    #[test]
    fn leading_region_is_fifty_bytes() {
        // The contiguous section-table region must be exactly 50 bytes
        // (5 × 10), starting at 0x2C and ending at 0x5E.
        assert_eq!(LEADING_SECTION_REGION_LEN, 50);
        assert_eq!(LEADING_SECTION_REGION_START, 0x2C);
        assert_eq!(LEADING_SECTION_REGION_END, 0x5E);
    }
}
