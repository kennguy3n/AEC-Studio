//! Auxiliary sentinel sections for R2004+: AcDb:AuxHeader,
//! AcDb:Template, AcDb:AppInfo.
//!
//! LibreDWG's `decode_R2004` codepath
//! (`decode.c::read_2004_section_*`) enumerates the section-info
//! descriptors by fixed-type id and dispatches to a per-section
//! reader. If a section is missing, it returns either
//! `DWG_ERR_SECTIONNOTFOUND` (>= 0x80 — critical, becomes process
//! `EXIT=1`) or `DWG_ERR_VALUEOUTOFBOUNDS` (< 0x80 — soft warning),
//! depending on `Dwg_Section_Type`. See decode.c:1959-1969:
//!
//! ```c
//! if (type < SECTION_REVHISTORY && type != SECTION_TEMPLATE
//!     && type != SECTION_OBJFREESPACE)
//!     return DWG_ERR_SECTIONNOTFOUND;
//! else
//!     return DWG_ERR_VALUEOUTOFBOUNDS;
//! ```
//!
//! `SECTION_AUXHEADER = 2` matches the strict path → its absence is
//! critical. `SECTION_TEMPLATE = 5` is exempted → soft warning only.
//! `SECTION_APPINFO = 11` is >= REVHISTORY (8) → soft warning only.
//!
//! This module emits valid (decode-clean) byte buffers for all three
//! so that:
//!
//! 1. R2004/R2010/R2013 stop returning `SECTIONNOTFOUND` (the missing
//!    AuxHeader was the sole critical error blocking dwgread EXIT=0
//!    after Phase 4).
//! 2. Template and AppInfo decode cleanly (no warnings) instead of
//!    being silently absent.
//!
//! All three are byte-aligned binary streams; despite using a
//! [`BitWriter`] for consistency with the rest of the codebase, no
//! sub-byte writes are emitted. The bit position therefore equals
//! `bytes_written * 8` at every field boundary.

use crate::dwg::bits::BitReader;
use crate::dwg::bits::BitWriter;
use crate::dwg::error::DwgResult;
use crate::dwg::version::Version;

/// LibreDWG `Dwg_Section_Type::SECTION_AUXHEADER` (`= 2`).
pub const SECTION_TYPE_AUXHEADER: u32 = 2;
/// LibreDWG `Dwg_Section_Type::SECTION_TEMPLATE` (`= 5`).
pub const SECTION_TYPE_TEMPLATE: u32 = 5;
/// LibreDWG `Dwg_Section_Type::SECTION_APPINFO` (`= 11`).
pub const SECTION_TYPE_APPINFO: u32 = 11;

/// LibreDWG-recognized section name strings (placed verbatim in the
/// 64-byte `SectionInfoDescriptor.name` field). The reader picks the
/// section reader by `fixedtype` after looking up the name, so the
/// strings here are the canonical AutoCAD ones for completeness /
/// future cross-tool compatibility.
pub const SECTION_NAME_AUXHEADER: &str = "AcDb:AuxHeader";
pub const SECTION_NAME_TEMPLATE: &str = "AcDb:Template";
pub const SECTION_NAME_APPINFO: &str = "AcDb:AppInfo";

/// AuxHeader section payload — mirrors `auxheader.spec`.
///
/// All numeric defaults match LibreDWG's `IF_ENCODE_FROM_EARLIER`
/// block (auxheader.spec:21-41): `aux_intro = {0xff, 0x77, 0x01}`,
/// `unknown_6rs = {4, 0x565, 0, 0, 2, 1}`,
/// `unknown_5rl = {0, 0, 0, 256, 393218}`, `minus_1 = -1`. The
/// version + maint fields are taken from the file header so AutoCAD's
/// recovery tools see a self-consistent dwg/aux version pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxHeaderSection {
    /// Stored as `RS dwg_version` (always 2 bytes regardless of file
    /// version). Equals `dwg->header.dwg_version` — an opaque byte
    /// identifying the encoder. We write the same value AutoCAD
    /// emits: matches the file-header `dwg_version` (= maintenance
    /// release byte for our self-authored files).
    pub dwg_version: u16,
    /// Stored as `RS maint_version` for R12-R2013, `RL` for R2018+.
    /// We always populate `u32` and let the encoder pick the width.
    pub maint_version: u32,
    /// Total saves counter (`RL numsaves`). 1 for the first save.
    pub numsaves: u32,
    /// `TIMERLL TDCREATE` — Julian-day pair. 0 means "unset" which
    /// AutoCAD interprets as the file's mtime.
    pub tdcreate: TimeRll,
    /// `TIMERLL TDUPDATE`.
    pub tdupdate: TimeRll,
    /// `RL HANDSEED` — next unused handle, mirrors the header-vars
    /// HANDSEED field.
    pub handseed: u32,
}

/// Pair of 32-bit unsigned values (Julian day + ms-since-midnight),
/// stored as `TIMERLL` in DWG. 8 bytes total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeRll {
    pub julian_day: u32,
    pub ms_into_day: u32,
}

impl AuxHeaderSection {
    /// Build an AuxHeader populated with the same defaults LibreDWG's
    /// encoder writes for a "first save" of a freshly-created file.
    pub fn fresh_for(version: Version) -> Self {
        Self {
            dwg_version: u16::from(version.maintenance_release()),
            maint_version: u32::from(version.maintenance_release()),
            numsaves: 1,
            tdcreate: TimeRll::default(),
            tdupdate: TimeRll::default(),
            handseed: 0x100,
        }
    }

    /// Serialize per auxheader.spec. Returns the encoded byte buffer
    /// (no CRC, no compression — that's the section's responsibility).
    pub fn encode(&self, version: Version) -> DwgResult<Vec<u8>> {
        let mut w = BitWriter::with_capacity(128);
        // FIELD_VECTOR_INL (aux_intro, RC, 3, 0) — { 0xff, 0x77, 0x01 }
        w.write_rc(0xff)?;
        w.write_rc(0x77)?;
        w.write_rc(0x01)?;
        // FIELD_RSx (dwg_version, 0)
        w.write_rs(self.dwg_version)?;
        // UNTIL (R_2013) : FIELD_CAST(maint_version, RS, RLx)  → RS (2 bytes)
        // SINCE (R_2018) : FIELD_RLx(maint_version)            → RL (4 bytes)
        write_cast_maint(&mut w, version, self.maint_version)?;
        // FIELD_RL (numsaves, 0)
        w.write_rl(self.numsaves)?;
        // FIELD_RLd (minus_1, 0) — i32 -1 little-endian = 0xFFFFFFFF
        w.write_rl(u32::MAX)?;
        // FIELD_RS (numsaves_1, 0), FIELD_RS (numsaves_2, 0)
        w.write_rs(0)?;
        w.write_rs(0)?;
        // FIELD_RL (zero, 0)
        w.write_rl(0)?;
        // FIELD_RSx (dwg_version_1, 0), FIELD_CAST(maint_version_1, RS, RLx)
        w.write_rs(self.dwg_version)?;
        write_cast_maint(&mut w, version, self.maint_version)?;
        // FIELD_RSx (dwg_version_2, 0), FIELD_CAST(maint_version_2, RS, RLx)
        w.write_rs(self.dwg_version)?;
        write_cast_maint(&mut w, version, self.maint_version)?;
        // FIELD_VECTOR_INL (unknown_6rs, RS, 6, 0) — {4, 0x565, 0, 0, 2, 1}
        for value in [4u16, 0x0565, 0, 0, 2, 1] {
            w.write_rs(value)?;
        }
        // FIELD_VECTOR_INL (unknown_5rl, RL, 5, 0) — {0, 0, 0, 256, 393218}
        for value in [0u32, 0, 0, 256, 393_218] {
            w.write_rl(value)?;
        }
        // FIELD_TIMERLL (TDCREATE, 0)
        w.write_rl(self.tdcreate.julian_day)?;
        w.write_rl(self.tdcreate.ms_into_day)?;
        // FIELD_TIMERLL (TDUPDATE, 0)
        w.write_rl(self.tdupdate.julian_day)?;
        w.write_rl(self.tdupdate.ms_into_day)?;
        // FIELD_RLx (HANDSEED, 0)
        w.write_rl(self.handseed)?;
        // FIELD_RL (plot_stamp, 0)
        w.write_rl(0)?;
        // FIELD_RS (zero_1, 0), FIELD_RS (numsaves_3, 0)
        w.write_rs(0)?;
        w.write_rs(0)?;
        // 6 × RL: zero_2, zero_3, zero_4, numsaves_4, zero_5, zero_6
        for _ in 0..6 {
            w.write_rl(0)?;
        }
        // SINCE (R_2004a) : zero_7, zero_8
        if version >= Version::R2004 {
            w.write_rl(0)?;
            w.write_rl(0)?;
        }
        // SINCE (R_2018) : unknown_18[3] RS
        if version >= Version::R2018 {
            w.write_rs(0)?;
            w.write_rs(0)?;
            w.write_rs(0)?;
        }
        Ok(w.into_bytes())
    }

    /// Parse the symmetric on-disk representation. Returns `Err` if
    /// the buffer is shorter than the version-specific minimum.
    pub fn parse(version: Version, bytes: &[u8]) -> DwgResult<Self> {
        let mut r = BitReader::new(bytes);
        // FIELD_VECTOR_INL aux_intro — 3 separate RC bytes.
        let _aux0 = read_rc(&mut r)?;
        let _aux1 = read_rc(&mut r)?;
        let _aux2 = read_rc(&mut r)?;
        let dwg_version = r.read_rs()?;
        let maint_version = read_cast_maint(&mut r, version)?;
        let numsaves = r.read_rl()?;
        let _minus_1 = r.read_rl()?;
        let _numsaves_1 = r.read_rs()?;
        let _numsaves_2 = r.read_rs()?;
        let _zero = r.read_rl()?;
        let _dwg_version_1 = r.read_rs()?;
        let _maint_version_1 = read_cast_maint(&mut r, version)?;
        let _dwg_version_2 = r.read_rs()?;
        let _maint_version_2 = read_cast_maint(&mut r, version)?;
        for _ in 0..6 {
            let _ = r.read_rs()?;
        }
        for _ in 0..5 {
            let _ = r.read_rl()?;
        }
        let tdcreate = TimeRll {
            julian_day: r.read_rl()?,
            ms_into_day: r.read_rl()?,
        };
        let tdupdate = TimeRll {
            julian_day: r.read_rl()?,
            ms_into_day: r.read_rl()?,
        };
        let handseed = r.read_rl()?;
        let _plot_stamp = r.read_rl()?;
        let _zero_1 = r.read_rs()?;
        let _numsaves_3 = r.read_rs()?;
        for _ in 0..6 {
            let _ = r.read_rl()?;
        }
        if version >= Version::R2004 {
            let _ = r.read_rl()?;
            let _ = r.read_rl()?;
        }
        if version >= Version::R2018 {
            let _ = r.read_rs()?;
            let _ = r.read_rs()?;
            let _ = r.read_rs()?;
        }
        Ok(Self {
            dwg_version,
            maint_version,
            numsaves,
            tdcreate,
            tdupdate,
            handseed,
        })
    }
}

/// AcDb:Template section — mirrors `template.spec`. Two fields:
/// a `T16 description` (string) and an `RS MEASUREMENT` (0 = English,
/// 1 = metric). LibreDWG decodes `description` with `bit_read_TU16`
/// for R2007+ (UTF-16LE) and `bit_read_T16` for R2004 (ASCII bytes).
/// See dec_macros.h:519 `FIELD_T16`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateSection {
    /// Free-form description string. Typically empty in
    /// AutoCAD-emitted files.
    pub description: String,
    /// `MEASUREMENT` — 0 = English (inches), 1 = metric (mm). Mirrored
    /// into `header_vars` after section parsing. Defaults to 0
    /// (English), matching the AutoCAD/Teigha convention for an
    /// uninitialized Template section.
    pub measurement: u16,
}

impl TemplateSection {
    pub fn encode(&self, version: Version) -> DwgResult<Vec<u8>> {
        let mut w = BitWriter::with_capacity(32);
        // FIELD_T16: pre-R2007 = RS length + N bytes ASCII;
        //            R2007+   = RS length (char count) + N RS chars (UTF-16LE).
        if version.uses_utf16_strings() {
            // bit_read_TU16: length = char count, then `length` RS chars.
            let chars: Vec<u16> = self.description.encode_utf16().collect();
            let len = u16::try_from(chars.len()).map_err(|_| {
                crate::dwg::error::DwgError::InternalInvariant(format!(
                    "template description char count {} > u16::MAX",
                    chars.len()
                ))
            })?;
            w.write_rs(len)?;
            for ch in chars {
                w.write_rs(ch)?;
            }
        } else {
            let bytes = self.description.as_bytes();
            let len = u16::try_from(bytes.len()).map_err(|_| {
                crate::dwg::error::DwgError::InternalInvariant(format!(
                    "template description byte count {} > u16::MAX",
                    bytes.len()
                ))
            })?;
            w.write_rs(len)?;
            for &b in bytes {
                w.write_rc(b)?;
            }
        }
        // FIELD_RS (MEASUREMENT, 0)
        w.write_rs(self.measurement)?;
        Ok(w.into_bytes())
    }

    pub fn parse(version: Version, bytes: &[u8]) -> DwgResult<Self> {
        let mut r = BitReader::new(bytes);
        let description = if version.uses_utf16_strings() {
            let len = r.read_rs()? as usize;
            let mut chars = Vec::with_capacity(len);
            for _ in 0..len {
                chars.push(r.read_rs()?);
            }
            String::from_utf16(&chars).map_err(|e| {
                crate::dwg::error::DwgError::InternalInvariant(format!(
                    "template description UTF-16 decode failed: {e}"
                ))
            })?
        } else {
            let len = r.read_rs()? as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push(read_rc(&mut r)?);
            }
            String::from_utf8(buf).map_err(|e| {
                crate::dwg::error::DwgError::InternalInvariant(format!(
                    "template description ASCII decode failed: {e}"
                ))
            })?
        };
        let measurement = r.read_rs()?;
        Ok(Self {
            description,
            measurement,
        })
    }
}

/// AcDb:AppInfo section — mirrors the `class_version >= 3` branch of
/// `appinfo.spec`. AutoCAD always writes class_version=3 in R2004+
/// files; the R2004-only `< 3` variant is for ancient Teigha
/// compatibility and is not emitted here.
///
/// Layout:
/// ```text
/// RL class_version            (= 3)
/// T16 appinfo_name            ("AppInfoDataList")
/// RL num_strings              (= 3)
/// TFFx[16] version_checksum   (16 raw bytes)
/// T16 version                 (e.g. "19.0.55.0.0")
/// TFFx[16] comment_checksum
/// T16 comment
/// ```
///
/// `bit_read_T16` semantics: pre-R2007 = RS length + length bytes
/// ASCII; R2007+ = RS length (char count) + length × RS chars
/// UTF-16LE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfoSection {
    /// `class_version` RL. AutoCAD writes 3 in R2004+.
    pub class_version: u32,
    /// `appinfo_name` T16. AutoCAD writes "AppInfoDataList".
    pub appinfo_name: String,
    /// `num_strings` RL. AutoCAD writes 3.
    pub num_strings: u32,
    /// `version_checksum` TFFx[16] — 16 raw bytes. AutoCAD writes a
    /// vendor-specific hash; we write 16 zero bytes.
    pub version_checksum: [u8; 16],
    /// `version` T16 — e.g. "19.0.55.0.0".
    pub version: String,
    /// `comment_checksum` TFFx[16].
    pub comment_checksum: [u8; 16],
    /// `comment` T16 — typically a "trusted DWG" banner string.
    pub comment: String,
}

impl Default for AppInfoSection {
    fn default() -> Self {
        Self {
            class_version: 3,
            appinfo_name: "AppInfoDataList".to_string(),
            num_strings: 3,
            version_checksum: [0u8; 16],
            version: String::new(),
            comment_checksum: [0u8; 16],
            comment: String::new(),
        }
    }
}

impl AppInfoSection {
    pub fn encode(&self, version: Version) -> DwgResult<Vec<u8>> {
        let mut w = BitWriter::with_capacity(128);
        // FIELD_RL (class_version, 0)
        w.write_rl(self.class_version)?;
        // FIELD_T16 (appinfo_name, 0)
        write_t16(&mut w, version, &self.appinfo_name)?;
        // FIELD_RL (num_strings, 0)
        w.write_rl(self.num_strings)?;
        // FIELD_TFFx (version_checksum, 16, 0) — 16 raw bytes
        for &b in &self.version_checksum {
            w.write_rc(b)?;
        }
        // FIELD_T16 (version, 0)
        write_t16(&mut w, version, &self.version)?;
        // FIELD_TFFx (comment_checksum, 16, 0)
        for &b in &self.comment_checksum {
            w.write_rc(b)?;
        }
        // FIELD_T16 (comment, 0)
        write_t16(&mut w, version, &self.comment)?;
        Ok(w.into_bytes())
    }

    pub fn parse(version: Version, bytes: &[u8]) -> DwgResult<Self> {
        let mut r = BitReader::new(bytes);
        let class_version = r.read_rl()?;
        let appinfo_name = read_t16(&mut r, version)?;
        let num_strings = r.read_rl()?;
        let mut version_checksum = [0u8; 16];
        for slot in &mut version_checksum {
            *slot = read_rc(&mut r)?;
        }
        let version_str = read_t16(&mut r, version)?;
        let mut comment_checksum = [0u8; 16];
        for slot in &mut comment_checksum {
            *slot = read_rc(&mut r)?;
        }
        let comment = read_t16(&mut r, version)?;
        Ok(Self {
            class_version,
            appinfo_name,
            num_strings,
            version_checksum,
            version: version_str,
            comment_checksum,
            comment,
        })
    }
}

/// `FIELD_CAST(maint_version, RS, RLx)` from auxheader.spec — RS
/// (2 bytes) for R12-R2013, RL (4 bytes) for R2018+.
fn write_cast_maint(w: &mut BitWriter, version: Version, value: u32) -> DwgResult<()> {
    if version >= Version::R2018 {
        w.write_rl(value)
    } else {
        let truncated = u16::try_from(value & 0xffff).expect("masked to u16");
        w.write_rs(truncated)
    }
}

fn read_cast_maint(r: &mut BitReader<'_>, version: Version) -> DwgResult<u32> {
    if version >= Version::R2018 {
        r.read_rl()
    } else {
        Ok(u32::from(r.read_rs()?))
    }
}

fn write_t16(w: &mut BitWriter, version: Version, s: &str) -> DwgResult<()> {
    if version.uses_utf16_strings() {
        let chars: Vec<u16> = s.encode_utf16().collect();
        let len = u16::try_from(chars.len()).map_err(|_| {
            crate::dwg::error::DwgError::InternalInvariant(format!(
                "T16 string char count {} > u16::MAX",
                chars.len()
            ))
        })?;
        w.write_rs(len)?;
        for ch in chars {
            w.write_rs(ch)?;
        }
    } else {
        let bytes = s.as_bytes();
        let len = u16::try_from(bytes.len()).map_err(|_| {
            crate::dwg::error::DwgError::InternalInvariant(format!(
                "T16 string byte count {} > u16::MAX",
                bytes.len()
            ))
        })?;
        w.write_rs(len)?;
        for &b in bytes {
            w.write_rc(b)?;
        }
    }
    Ok(())
}

fn read_t16(r: &mut BitReader<'_>, version: Version) -> DwgResult<String> {
    let len = r.read_rs()? as usize;
    if version.uses_utf16_strings() {
        let mut chars = Vec::with_capacity(len);
        for _ in 0..len {
            chars.push(r.read_rs()?);
        }
        String::from_utf16(&chars).map_err(|e| {
            crate::dwg::error::DwgError::InternalInvariant(format!("T16 UTF-16 decode failed: {e}"))
        })
    } else {
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            buf.push(read_rc(r)?);
        }
        String::from_utf8(buf).map_err(|e| {
            crate::dwg::error::DwgError::InternalInvariant(format!("T16 ASCII decode failed: {e}"))
        })
    }
}

fn read_rc(r: &mut BitReader<'_>) -> DwgResult<u8> {
    let v = r.read_bits_u32(8)?;
    Ok(v as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxheader_round_trips_r2004() {
        let aux = AuxHeaderSection::fresh_for(Version::R2004);
        let bytes = aux.encode(Version::R2004).unwrap();
        let back = AuxHeaderSection::parse(Version::R2004, &bytes).unwrap();
        assert_eq!(aux, back);
    }

    #[test]
    fn auxheader_round_trips_r2010() {
        let aux = AuxHeaderSection::fresh_for(Version::R2010);
        let bytes = aux.encode(Version::R2010).unwrap();
        let back = AuxHeaderSection::parse(Version::R2010, &bytes).unwrap();
        assert_eq!(aux, back);
    }

    #[test]
    fn auxheader_round_trips_r2013() {
        let aux = AuxHeaderSection::fresh_for(Version::R2013);
        let bytes = aux.encode(Version::R2013).unwrap();
        let back = AuxHeaderSection::parse(Version::R2013, &bytes).unwrap();
        assert_eq!(aux, back);
    }

    #[test]
    fn auxheader_round_trips_r2018() {
        let aux = AuxHeaderSection::fresh_for(Version::R2018);
        let bytes = aux.encode(Version::R2018).unwrap();
        let back = AuxHeaderSection::parse(Version::R2018, &bytes).unwrap();
        assert_eq!(aux, back);
    }

    #[test]
    fn auxheader_intro_is_libredwg_constant() {
        // First 3 bytes must be {0xff, 0x77, 0x01} for any version.
        for &v in &[
            Version::R2004,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            let bytes = AuxHeaderSection::fresh_for(v).encode(v).unwrap();
            assert_eq!(
                &bytes[0..3],
                &[0xff, 0x77, 0x01],
                "intro mismatch for {v:?}"
            );
        }
    }

    #[test]
    fn auxheader_maint_width_matches_version() {
        // R2013: maint_version = RS (2 bytes) → offset 3 + RS(dwg_version=2)
        // = 5 occupied; maint_version sits at byte 5 as 2 LE bytes.
        let aux_r2013 = AuxHeaderSection {
            dwg_version: 0x1f,
            maint_version: 9,
            ..AuxHeaderSection::fresh_for(Version::R2013)
        };
        let bytes = aux_r2013.encode(Version::R2013).unwrap();
        // Total expected size for R2013 (no R2018 padding, with R2004+ zeros 7/8):
        //   3 (intro) + 2 (dwg_v) + 2 (maint) + 4 (numsaves) + 4 (minus_1)
        // + 2 + 2 (numsaves_1,2) + 4 (zero) + 2+2 (dwg_v_1, maint_1)
        // + 2+2 (dwg_v_2, maint_2) + 12 (6×RS unknown_6rs)
        // + 20 (5×RL unknown_5rl) + 16 (TIMERLL×2) + 4 (HANDSEED)
        // + 4 (plot_stamp) + 2+2 (zero_1, numsaves_3) + 24 (6×RL)
        // + 8 (R2004+ zero_7, zero_8) = 123 bytes
        assert_eq!(bytes.len(), 123);
        // Maint at offset 5..7 (after intro+dwg_v).
        assert_eq!(&bytes[5..7], &9u16.to_le_bytes());

        // R2018: maint_version = RL (4 bytes) → offset 5..9.
        let aux_r2018 = AuxHeaderSection {
            dwg_version: 0x21,
            maint_version: 1,
            ..AuxHeaderSection::fresh_for(Version::R2018)
        };
        let bytes = aux_r2018.encode(Version::R2018).unwrap();
        assert_eq!(&bytes[5..9], &1u32.to_le_bytes());
    }

    #[test]
    fn template_round_trips_empty_r2004() {
        let tpl = TemplateSection::default();
        let bytes = tpl.encode(Version::R2004).unwrap();
        // RS length=0 (2 bytes) + RS MEASUREMENT=0 (2 bytes) = 4 bytes.
        assert_eq!(bytes.len(), 4);
        let back = TemplateSection::parse(Version::R2004, &bytes).unwrap();
        assert_eq!(tpl, back);
    }

    #[test]
    fn template_round_trips_utf16_r2010() {
        let tpl = TemplateSection {
            description: "metric".to_string(),
            measurement: 1,
        };
        let bytes = tpl.encode(Version::R2010).unwrap();
        // RS length=6 (2 bytes) + 6×RS UTF-16 (12 bytes) + RS measurement (2 bytes).
        assert_eq!(bytes.len(), 16);
        let back = TemplateSection::parse(Version::R2010, &bytes).unwrap();
        assert_eq!(tpl, back);
    }

    #[test]
    fn template_round_trips_ascii_r2004() {
        let tpl = TemplateSection {
            description: "English".to_string(),
            measurement: 0,
        };
        let bytes = tpl.encode(Version::R2004).unwrap();
        // RS length=7 (2 bytes) + 7×RC ASCII (7 bytes) + RS measurement (2 bytes).
        assert_eq!(bytes.len(), 11);
        let back = TemplateSection::parse(Version::R2004, &bytes).unwrap();
        assert_eq!(tpl, back);
    }

    #[test]
    fn appinfo_round_trips_r2004() {
        let app = AppInfoSection::default();
        let bytes = app.encode(Version::R2004).unwrap();
        let back = AppInfoSection::parse(Version::R2004, &bytes).unwrap();
        assert_eq!(app, back);
    }

    #[test]
    fn appinfo_round_trips_r2010() {
        let app = AppInfoSection::default();
        let bytes = app.encode(Version::R2010).unwrap();
        let back = AppInfoSection::parse(Version::R2010, &bytes).unwrap();
        assert_eq!(app, back);
    }

    #[test]
    fn appinfo_round_trips_with_strings_r2013() {
        let app = AppInfoSection {
            class_version: 3,
            appinfo_name: "AppInfoDataList".to_string(),
            num_strings: 3,
            version_checksum: [0xde; 16],
            version: "19.0.55.0.0".to_string(),
            comment_checksum: [0xad; 16],
            comment: "Autodesk DWG.".to_string(),
        };
        let bytes = app.encode(Version::R2013).unwrap();
        let back = AppInfoSection::parse(Version::R2013, &bytes).unwrap();
        assert_eq!(app, back);
    }

    #[test]
    fn appinfo_class_version_is_at_offset_0() {
        let app = AppInfoSection::default();
        let bytes = app.encode(Version::R2010).unwrap();
        assert_eq!(&bytes[0..4], &3u32.to_le_bytes());
    }
}
