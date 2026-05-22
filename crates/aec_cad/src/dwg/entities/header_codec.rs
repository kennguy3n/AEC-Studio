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
use crate::dwg::error::{DwgError, DwgResult};
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
///
/// Field ordering and version gating exactly mirror LibreDWG's
/// `common_entity_data.spec`. The R14, R2000, R2004, R2007, R2010,
/// R2013, R2018 boundaries each shift a handful of bits; see the
/// encode/decode implementations for the exact wire format.
#[derive(Debug, Clone, PartialEq)]
pub struct CommonHeaderData {
    /// True iff an image preview blob is present (B at start of the
    /// common entity data for SINCE R_13b1). We never emit a preview
    /// for our synthesized entities, so this is always `false` on the
    /// write side, but the decoder still has to honor the bit.
    pub preview_exists: bool,
    pub entity_mode: EntityMode,
    pub reactor_count: u32,
    /// R14/R2000 only: "is by-layer linetype" companion bit (B).
    /// LibreDWG calls this `isbylayerlt` and derives `ltype_flags`
    /// from it for VERSIONS R_13b1..R_14. Stored here so a round-trip
    /// produces identical bytes on those legacy versions.
    pub isbylayerlt: bool,
    /// R14..R2002 only: "no links" flag (B). Has no effect on the
    /// modern R2004+ branch.
    pub nolinks: bool,
    /// R2013+ only: bit indicating an attached DS (drawing-state)
    /// data block. We never emit DS data, so this is always false.
    pub has_ds_data: bool,
    /// Color in LibreDWG's CMC (≤ R2002) / ENC (R2004+) encoding.
    /// `raw` is the 16-bit BSx field; the conditional alpha / handle /
    /// rgb / book-name fields are NOT yet modeled — we only emit
    /// values whose `flag = raw >> 8` is 0, 1, or 0x100 (palette
    /// index, including the BYLAYER magic index 256).
    pub color_raw: u16,
    pub linetype_scale: f64,
    pub linetype_flag: LinetypeFlag,
    pub plot_style_flag: u8, // BB
    /// R2007+ material flag (BB). Mirrors `plot_style_flag`'s shape:
    /// 0b00 = BYLAYER, 0b01 = BYBLOCK, 0b10 = continuous (unused),
    /// 0b11 = handle. When `0b11`, a material handle is appended to
    /// the handle stream after plot-style. The encoder/decoder for the
    /// data stream always write/read this BB for R2007+; the handle
    /// stream emits/consumes a handle iff this flag is 0b11.
    pub material_flag: u8,
    /// R2007+ shadow flags (RC0). 0 both, 1 receives, 2 casts, 3 no.
    pub shadow_flags: u8,
    /// R2010+ visual-style flags (3 separate B bits).
    pub has_full_visualstyle: bool,
    pub has_face_visualstyle: bool,
    pub has_edge_visualstyle: bool,
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
            preview_exists: false,
            entity_mode: EntityMode::BlockHeader,
            reactor_count: 0,
            isbylayerlt: false,
            nolinks: true,
            has_ds_data: false,
            // Default BYLAYER color: raw=0x100 → flag=1 (no extras),
            // index = 0x100 & 0x1ff = 256 = BYLAYER magic.
            color_raw: 0x100,
            linetype_scale: 1.0,
            linetype_flag: LinetypeFlag::ByLayer,
            plot_style_flag: 0,
            material_flag: 0,
            shadow_flags: 0,
            has_full_visualstyle: false,
            has_face_visualstyle: false,
            has_edge_visualstyle: false,
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
    ///
    /// Field order matches LibreDWG `common_entity_data.spec` exactly:
    ///
    /// 1. `preview_exists` (B). We never set this true, so the
    ///    optional preview block is skipped.
    /// 2. (R14 only, handled by record framing) inline `bitsize` RL.
    /// 3. `entmode` (BB) and `num_reactors` (BL).
    /// 4. R14: `isbylayerlt` (B) | R2004+: `is_xdic_missing` (B).
    /// 5. R14..R2002: `nolinks` (B) | R2013+: `has_ds_data` (B).
    /// 6. R2004+: color ENC `color.raw` (BSx 16) | else: CMC.
    /// 7. `linetype_scale` (BD1).
    /// 8. R2000+: `ltype_flags` (BB), `plotstyle_flags` (BB).
    /// 9. R2007+: `material_flags` (BB), `shadow_flags` (RC0).
    /// 10. R2010+: 3 visual-style flag bits (B).
    /// 11. `invisible` (BS), R2000+ `linewt` (RC).
    pub fn encode_for_version(&self, version: Version, w: &mut BitWriter) -> DwgResult<()> {
        // Step 1: preview_exists. We don't carry preview blobs, so
        // we always emit `false` and skip the conditional block.
        w.write_b(self.preview_exists)?;
        if self.preview_exists {
            return Err(DwgError::Unsupported(
                "image-preview emit on entities is not yet wired through CommonHeaderData".into(),
            ));
        }

        // Step 3: entmode + num_reactors.
        w.write_bb(self.entity_mode.to_bb())?;
        w.write_bl(i64::from(self.reactor_count))?;

        // Step 4: isbylayerlt (R_13b1..R_14) | is_xdic_missing (R2004+).
        if version <= Version::R14 {
            w.write_b(self.isbylayerlt)?;
        } else if version >= Version::R2004 {
            w.write_b(self.xdict_missing)?;
        }

        // Step 5: nolinks (R_13b1..R_2002) | has_ds_data (R2013+).
        if version <= Version::R2000 {
            w.write_b(self.nolinks)?;
        } else if version >= Version::R2013 {
            w.write_b(self.has_ds_data)?;
        }

        // Step 6: color encoding. R2004+ uses ENC (BSx raw 16);
        // pre-R2004 uses CMC (BS index). LibreDWG ENC has additional
        // conditional fields gated on the flag bits (0x20=alpha,
        // 0x40=handle, 0x80=rgb, 0x41=name, 0x42=book-name); we only
        // emit values whose flag is 0, 1, or 1 (BYLAYER index=256
        // → raw=0x100 → flag=1 with no extras). Any flag with the
        // extra-info bits set is rejected at encode time.
        if version >= Version::R2004 {
            let flag = (self.color_raw >> 8) as u8;
            if flag & 0xE2 != 0 {
                return Err(DwgError::Unsupported(format!(
                    "entity color ENC flag 0x{flag:02X} (alpha/handle/rgb/name) is not yet supported",
                )));
            }
            w.write_bs(i32::from(self.color_raw))?;
        } else {
            // CMC: emit the raw color index as BS. LibreDWG's CMC for
            // pre-R2004 is `BS color.index, then conditional book
            // fields gated on flag bits`. We only ever emit BYLAYER
            // (index 256) for now, matching our default.
            w.write_bs(i32::from(self.color_raw & 0x1ff))?;
        }

        // Step 7: linetype scale (BD1).
        w.write_bd(self.linetype_scale)?;

        // Step 8: ltype_flags + plotstyle_flags (R2000+).
        if version >= Version::R2000 {
            w.write_bb(self.linetype_flag.to_bb())?;
            w.write_bb(self.plot_style_flag)?;
        }

        // Step 9: material_flags + shadow_flags (R2007+).
        if version >= Version::R2007 {
            if self.material_flag > 0b11 {
                return Err(DwgError::InternalInvariant(format!(
                    "material_flag {} exceeds 2 bits",
                    self.material_flag
                )));
            }
            w.write_bb(self.material_flag)?;
            w.write_bits_u32(8, u32::from(self.shadow_flags))?;
        }

        // Step 10: 3 visual-style B bits (R2010+).
        if version >= Version::R2010 {
            w.write_b(self.has_full_visualstyle)?;
            w.write_b(self.has_face_visualstyle)?;
            w.write_b(self.has_edge_visualstyle)?;
        }

        // Step 11: invisibility (BS) + linewt (RC for R2000+).
        w.write_bs(self.invisibility)?;
        if version >= Version::R2000 {
            w.write_bits_u32(8, u32::from(self.lineweight))?;
        }
        Ok(())
    }

    /// Decode the data-stream portion of the common entity header.
    pub fn decode_for_version(version: Version, r: &mut BitReader<'_>) -> DwgResult<Self> {
        let preview_exists = r.read_b()?;
        if preview_exists {
            return Err(DwgError::Unsupported(
                "image-preview decode on entities is not yet wired through CommonHeaderData".into(),
            ));
        }
        let entity_mode = EntityMode::from_bb(r.read_bb()?);
        let reactor_count = r.read_bl()? as u32;
        let (isbylayerlt, xdict_missing) = if version <= Version::R14 {
            (r.read_b()?, true)
        } else if version >= Version::R2004 {
            (false, r.read_b()?)
        } else {
            (false, true)
        };
        let (nolinks, has_ds_data) = if version <= Version::R2000 {
            // R14/R2000: nolinks is on the wire; has_ds_data is not.
            (r.read_b()?, false)
        } else if version >= Version::R2013 {
            // R2013+: has_ds_data is on the wire; nolinks is not, so
            // we synthesize its struct-default (true) for round-trip
            // determinism.
            (true, r.read_b()?)
        } else {
            // R2004/R2007/R2010: neither bit is on the wire; defaults.
            (true, false)
        };
        let color_raw = if version >= Version::R2004 {
            let raw = r.read_bs()? as u16;
            let flag = (raw >> 8) as u8;
            if flag & 0xE2 != 0 {
                return Err(DwgError::Unsupported(format!(
                    "entity color ENC flag 0x{flag:02X} (alpha/handle/rgb/name) is not yet decoded",
                )));
            }
            raw
        } else {
            (r.read_bs()? as u16) & 0x1ff
        };
        let linetype_scale = r.read_bd()?;
        let (linetype_flag, plot_style_flag) = if version >= Version::R2000 {
            (LinetypeFlag::from_bb(r.read_bb()?), r.read_bb()?)
        } else {
            (LinetypeFlag::ByLayer, 0)
        };
        let (material_flag, shadow_flags) = if version >= Version::R2007 {
            let m = r.read_bb()?;
            let s = r.read_bits_u32(8)? as u8;
            (m, s)
        } else {
            (0, 0)
        };
        let (has_full_visualstyle, has_face_visualstyle, has_edge_visualstyle) =
            if version >= Version::R2010 {
                (r.read_b()?, r.read_b()?, r.read_b()?)
            } else {
                (false, false, false)
            };
        let invisibility = r.read_bs()?;
        let lineweight = if version >= Version::R2000 {
            r.read_bits_u32(8)? as u8
        } else {
            0x1d
        };
        Ok(Self {
            preview_exists,
            entity_mode,
            reactor_count,
            isbylayerlt,
            nolinks,
            has_ds_data,
            color_raw,
            linetype_scale,
            linetype_flag,
            plot_style_flag,
            material_flag,
            shadow_flags,
            has_full_visualstyle,
            has_face_visualstyle,
            has_edge_visualstyle,
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
        // Pre-R2004 CMC: raw value is the bare palette index (low 9
        // bits). R2004+ ENC: raw must have flag bits 0x20/0x40/0x80
        // clear to avoid the conditional alpha/handle/rgb fields
        // (which we don't yet emit).
        let header_legacy = CommonHeaderData {
            color_raw: 5, // blue palette index
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2000, &header_legacy);
        let header_modern = CommonHeaderData {
            // Plain palette index 5, flag bits clear.
            color_raw: 5,
            ..CommonHeaderData::default()
        };
        check_round_trip(Version::R2010, &header_modern);
        check_round_trip(Version::R2018, &header_modern);
    }

    #[test]
    fn entmode_paper_space_round_trips() {
        // "Paper space ownership" is now encoded via the BB entmode
        // (model/paper/no-owner/reserved), not via a separate B flag.
        // This test pins that round-tripping a paper-space entity
        // preserves the BB value.
        let header = CommonHeaderData {
            entity_mode: EntityMode::DistinctBlockHeader,
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

    #[test]
    fn material_flag_round_trips_for_r2007_plus() {
        // material_flag is meaningful from R2007 onward. When set to
        // 0b11 ("by handle") the handle stream emits a material H;
        // here we only check the common-header BB round-trips on its
        // own. Pre-R2007 versions don't carry this field, so we expect
        // it to come back as the default 0b00.
        for flag in [0b00, 0b01, 0b10, 0b11] {
            let header = CommonHeaderData {
                material_flag: flag,
                ..CommonHeaderData::default()
            };
            check_round_trip(Version::R2007, &header);
            check_round_trip(Version::R2010, &header);
            check_round_trip(Version::R2018, &header);
        }
    }

    #[test]
    fn material_flag_pre_r2007_falls_back_to_zero() {
        // R14/R2000 do not carry the material BB on the wire, so any
        // input flag is dropped to 0 on round-trip.
        let header = CommonHeaderData {
            material_flag: 0b11,
            ..CommonHeaderData::default()
        };
        let mut w = BitWriter::new();
        header.encode_for_version(Version::R2000, &mut w).unwrap();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        let got = CommonHeaderData::decode_for_version(Version::R2000, &mut r).unwrap();
        assert_eq!(got.material_flag, 0);
    }
}
