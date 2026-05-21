//! Common entity header bit-level codec for R2000+ DWG files.
//!
//! The on-disk layout of every entity in the OBJECTS section is split
//! into two bit streams: a **data stream** containing the entity's
//! values, and a **handle stream** containing every cross-reference
//! handle the entity needs (layer, linetype, owner, reactors, etc.).
//! The handle stream physically follows the data stream within the
//! same object record.
//!
//! This module owns the data-stream portion of the **common entity
//! header** — every modern DWG entity is prefixed with this block
//! regardless of type. The per-type payload (e.g. `LineEntity`)
//! follows the common header in the same data stream. The handle
//! stream is handled by [`super::record`].
//!
//! Reference: OpenDesign Specification § "Common Entity Data" and
//! LibreDWG `dwg.spec` macro `DWG_COMMON_ENTITY_HEADER`.
//!
//! We model the **R2000** shape here (which covers R14 with one extra
//! flag bit ignored). R2007+, R2010+, R2013+, and R2018+ add small
//! additional flag bits; those are layered on top via
//! [`encode_for_version`] / [`decode_for_version`].

use crate::dwg::bits::{BitReader, BitWriter};
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;

/// Entity mode bits (BB control bits in the common header).
///
/// `0b00` — owner is the model-space or paper-space block-header (which
/// one depends on the `paper_space` flag).
/// `0b01` — owner is a different block-header (handle code 0x04 follows).
/// `0b10` — no owner handle (e.g. dictionary-owned entities).
/// `0b11` — reserved / unused in modern AutoCAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityMode {
    BlockHeader = 0b00,
    DistinctBlockHeader = 0b01,
    NoOwner = 0b10,
    Reserved = 0b11,
}

impl EntityMode {
    pub fn to_bb(self) -> u8 {
        self as u8
    }
    pub fn from_bb(bb: u8) -> Self {
        match bb & 0b11 {
            0b00 => Self::BlockHeader,
            0b01 => Self::DistinctBlockHeader,
            0b10 => Self::NoOwner,
            _ => Self::Reserved,
        }
    }
}

/// Linetype flag bits in the common header.
///
/// `0b00` — linetype is BYLAYER.
/// `0b01` — linetype is BYBLOCK.
/// `0b10` — linetype is CONTINUOUS.
/// `0b11` — explicit linetype handle follows in the handle stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinetypeFlag {
    ByLayer = 0b00,
    ByBlock = 0b01,
    Continuous = 0b10,
    Handle = 0b11,
}

impl LinetypeFlag {
    pub fn to_bb(self) -> u8 {
        self as u8
    }
    pub fn from_bb(bb: u8) -> Self {
        match bb & 0b11 {
            0b00 => Self::ByLayer,
            0b01 => Self::ByBlock,
            0b10 => Self::Continuous,
            _ => Self::Handle,
        }
    }
}

/// Data-stream portion of the common entity header.
#[derive(Debug, Clone, PartialEq)]
pub struct CommonHeaderData {
    pub entity_mode: EntityMode,
    pub reactor_count: u32,
    /// True iff `paper_space` flag is set (entity owned by paper-space
    /// block header instead of model-space).
    pub paper_space: bool,
    pub is_color_by_layer: bool,
    /// Only meaningful when `is_color_by_layer = false`.
    pub color_aci: i32,
    pub linetype_scale: f64,
    pub linetype_flag: LinetypeFlag,
    pub plot_style_flag: u8, // BB
    /// R2004+ extension-dictionary-missing flag. When `true` the
    /// handle stream does NOT carry an xdict handle; when `false` the
    /// handle stream carries one extra `H` for the extension
    /// dictionary owner. The bit is written only for R2004+; for older
    /// versions xdict is always implicitly missing.
    pub xdict_missing: bool,
    /// Invisibility bitfield (R14+; bit 0 = "entity is invisible").
    pub invisibility: i32,
    /// LineWeight (RC). Values 0-211 are millimetres × 0.05. 0x1d ("by
    /// layer") is the default.
    pub lineweight: u8,
}

impl Default for CommonHeaderData {
    fn default() -> Self {
        Self {
            entity_mode: EntityMode::BlockHeader,
            reactor_count: 0,
            paper_space: false,
            is_color_by_layer: true,
            color_aci: 256,
            linetype_scale: 1.0,
            linetype_flag: LinetypeFlag::ByLayer,
            plot_style_flag: 0,
            xdict_missing: true,
            invisibility: 0,
            lineweight: 0x1d, // BYLAYER
        }
    }
}

impl CommonHeaderData {
    /// Encode the data-stream portion of the common entity header into
    /// `w` for the given DWG version. The handle stream — which
    /// follows immediately — is encoded separately by the caller.
    pub fn encode_for_version(&self, version: Version, w: &mut BitWriter) -> DwgResult<()> {
        // BB Entity Mode
        w.write_bb(self.entity_mode.to_bb())?;
        // BL reactor count
        w.write_bl(i64::from(self.reactor_count))?;
        // R2004+: B xdict-missing-flag. The handle stream emits an
        // extra `H` for the extension dictionary owner iff this flag
        // is `false`. R2013+ also adds the "has binary data" bit.
        if version >= Version::R2004 {
            w.write_b(self.xdict_missing)?;
        }
        if version >= Version::R2013 {
            w.write_b(false)?; // has binary data
        }
        // B paper_space flag
        w.write_b(self.paper_space)?;
        // B color-by-layer flag
        w.write_b(self.is_color_by_layer)?;
        if !self.is_color_by_layer {
            w.write_bs(self.color_aci)?;
        }
        w.write_bd(self.linetype_scale)?;
        w.write_bb(self.linetype_flag.to_bb())?;
        w.write_bb(self.plot_style_flag)?;
        // R2007+: material flag (BB), shadow flag (B).
        if version >= Version::R2007 {
            w.write_bb(0)?; // material BYLAYER
            w.write_b(false)?; // shadow flag
        }
        // R2010+: has full visual style (BB).
        if version >= Version::R2010 {
            w.write_bb(0)?;
        }
        // R2013+: 4 more bits of visual-style flags.
        if version >= Version::R2013 {
            w.write_b(false)?; // face-visual
            w.write_b(false)?; // edge-visual
            w.write_b(false)?; // unknown
            w.write_b(false)?; // unknown
        }
        // R2018+: one more flag bit.
        if version >= Version::R2018 {
            w.write_b(false)?;
        }
        // BS invisibility (R14+; for R12 a separate codepath handles
        // this — modern versions always include it).
        w.write_bs(self.invisibility)?;
        // RC lineweight (R2000+).
        if version >= Version::R2000 {
            w.write_bits_u32(8, u32::from(self.lineweight))?;
        }
        Ok(())
    }

    /// Decode the data-stream portion of the common entity header.
    pub fn decode_for_version(version: Version, r: &mut BitReader<'_>) -> DwgResult<Self> {
        let entity_mode = EntityMode::from_bb(r.read_bb()?);
        let reactor_count = r.read_bl()? as u32;
        let xdict_missing = if version >= Version::R2004 {
            r.read_b()?
        } else {
            // R14/R2000 never emit the flag; xdict is implicitly
            // "missing" so the handle stream skips the extra `H`.
            true
        };
        if version >= Version::R2013 {
            let _has_binary_data = r.read_b()?;
        }
        let paper_space = r.read_b()?;
        let is_color_by_layer = r.read_b()?;
        let color_aci = if is_color_by_layer { 256 } else { r.read_bs()? };
        let linetype_scale = r.read_bd()?;
        let linetype_flag = LinetypeFlag::from_bb(r.read_bb()?);
        let plot_style_flag = r.read_bb()?;
        if version >= Version::R2007 {
            let _material = r.read_bb()?;
            let _shadow = r.read_b()?;
        }
        if version >= Version::R2010 {
            let _visual_style = r.read_bb()?;
        }
        if version >= Version::R2013 {
            let _face = r.read_b()?;
            let _edge = r.read_b()?;
            let _u1 = r.read_b()?;
            let _u2 = r.read_b()?;
        }
        if version >= Version::R2018 {
            let _f = r.read_b()?;
        }
        let invisibility = r.read_bs()?;
        let lineweight = if version >= Version::R2000 {
            r.read_bits_u32(8)? as u8
        } else {
            0x1d
        };
        Ok(Self {
            entity_mode,
            reactor_count,
            paper_space,
            is_color_by_layer,
            color_aci,
            linetype_scale,
            linetype_flag,
            plot_style_flag,
            xdict_missing,
            invisibility,
            lineweight,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_round_trip(version: Version, header: &CommonHeaderData) {
        let mut w = BitWriter::new();
        header
            .encode_for_version(version, &mut w)
            .expect("encode must succeed");
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got =
            CommonHeaderData::decode_for_version(version, &mut r).expect("decode must succeed");
        assert_eq!(got, *header, "round-trip mismatch on {version:?}");
    }

    #[test]
    fn default_header_round_trips_on_r2000() {
        check_round_trip(Version::R2000, &CommonHeaderData::default());
    }

    #[test]
    fn default_header_round_trips_on_r2010() {
        check_round_trip(Version::R2010, &CommonHeaderData::default());
    }

    #[test]
    fn default_header_round_trips_on_r2018() {
        check_round_trip(Version::R2018, &CommonHeaderData::default());
    }

    #[test]
    fn explicit_color_round_trips() {
        let header = CommonHeaderData {
            is_color_by_layer: false,
            color_aci: 5, // blue
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
        check_round_trip(Version::R2010, &header);
        check_round_trip(Version::R2018, &header);
    }

    #[test]
    fn paper_space_round_trips() {
        let header = CommonHeaderData {
            paper_space: true,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
    }

    #[test]
    fn invisibility_and_lineweight_round_trip() {
        let header = CommonHeaderData {
            invisibility: 1,
            lineweight: 50,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
        check_round_trip(Version::R2010, &header);
    }

    #[test]
    fn nonzero_reactor_count_round_trips() {
        let header = CommonHeaderData {
            reactor_count: 3,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
    }

    #[test]
    fn linetype_handle_flag_round_trips() {
        let header = CommonHeaderData {
            linetype_flag: LinetypeFlag::Handle,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
    }

    #[test]
    fn linetype_scale_nondefault_round_trips() {
        let header = CommonHeaderData {
            linetype_scale: 2.5,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header);
    }
}
