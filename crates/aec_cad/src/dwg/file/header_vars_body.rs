//! Real `Dwg_Header_Variables` body encoder.
//!
//! LibreDWG's R14+ AcDb:Header section contains ~150 bit-packed system
//! variables, listed (and version-gated) by `header_variables.spec` in
//! the upstream source tree. This module ports the **encode** side of
//! that spec, so any modern DWG we write contains the same field set
//! LibreDWG will then decode without buffer-overflow errors.
//!
//! ## Field defaults
//!
//! For the moment we expose only a handful of programmable variables
//! through [`HeaderVars`] — the ones our pipeline actually configures
//! (drawing extents, current layer, code page, etc.). Everything else
//! is encoded with sensible defaults (zero/false/empty/null-handle).
//! When a downstream component needs to round-trip an additional
//! variable, add it to [`HeaderVars`] and patch the corresponding
//! `vars.field` site in [`emit_fields`]. No spec re-derivation is
//! required.
//!
//! ## Stream layout for R2007+
//!
//! For R13–R2004, LibreDWG's encoder writes the entire variable set
//! into a single bit stream (`sec_dat` aliased to `hdl_dat` and
//! `str_dat`). For R2007+ the decoder splits the section into three
//! sub-streams within the bit-encoded section blob:
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────┐
//! │ 16-byte sentinel (HEADER_VARS_BEGIN)                           │
//! ├────────────────────────────────────────────────────────────────┤
//! │ RL  size (4 bytes)                                             │
//! ├────────────────────────────────────────────────────────────────┤
//! │ (R2010+ with maint>3 or R2018+):                               │
//! │   RL bitsize_hi (4 bytes)                                      │
//! ├────────────────────────────────────────────────────────────────┤
//! │ RL  bitsize (4 bytes)                                          │
//! ├────────────────────────────────────────────────────────────────┤
//! │ ⬆ main "sec_dat" forward bit stream (numeric fields)           │
//! │ ⬇ "str_dat" reverse bit stream (T-typed strings)               │
//! │ ├─ RS data_size (16 bits)  ──┐                                 │
//! │ └─ B  endbit (1 bit, = 1)   ─┘                                 │
//! ├────────────────────────────────────────────────────────────────┤
//! │ "hdl_dat" forward bit stream (FIELD_HANDLE values)             │
//! ├────────────────────────────────────────────────────────────────┤
//! │ Pad to byte; CRC-X25 (2 bytes); 16-byte HEADER_VARS_END        │
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! `bitsize` covers the main + string region only; `hdl_dat` follows
//! immediately after. The exact semantics are mirrored from
//! `decode.c:2330` (`bit_set_position (&hdl_dat, endbits)`) and
//! `bits.c:section_string_stream`.

use crate::dwg::bits::reader::{Color, HandleRef};
use crate::dwg::bits::{crc_x25, BitWriter};
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::version::Version;

/// The full set of header variables this codec can encode. Most are
/// kept at their default values; the user-configurable subset is
/// exposed as `pub` fields so callers (and our golden-file tests) can
/// pin specific values. The fields not listed here are all emitted with
/// zero/false/empty defaults that LibreDWG accepts.
#[derive(Debug, Clone)]
pub struct HeaderVars {
    /// `REQUIREDVERSIONS` BLL (R2013+). Defaults to 0.
    pub required_versions: u64,
    /// `INSBASE` 3BD — drawing insertion origin. Defaults to (0,0,0).
    pub insbase: [f64; 3],
    /// `EXTMIN` 3BD — minimum drawing extent. Defaults to (0,0,0).
    pub extmin: [f64; 3],
    /// `EXTMAX` 3BD — maximum drawing extent. Defaults to (0,0,0).
    pub extmax: [f64; 3],
    /// `LIMMIN` 2RD — minimum drawing limits (paper). Defaults to (0,0).
    pub limmin: [f64; 2],
    /// `LIMMAX` 2RD — maximum drawing limits (paper). Defaults to (12,9).
    pub limmax: [f64; 2],
    /// `LTSCALE` BD — global linetype scale. Defaults to 1.0.
    pub ltscale: f64,
    /// `TEXTSIZE` BD — default text height. Defaults to 0.2.
    pub textsize: f64,
    /// `HANDSEED` — next unused handle. Defaults to handle 0x100.
    pub handseed: HandleRef,
    /// `CLAYER` — current layer handle. Defaults to null handle.
    pub clayer: HandleRef,
    /// `TEXTSTYLE` — current text style handle. Defaults to null.
    pub textstyle: HandleRef,
    /// `CELTYPE` — current linetype handle. Defaults to null.
    pub celtype: HandleRef,
    /// `CECOLOR` — current entity color. Defaults to ByLayer.
    pub cecolor: Color,
    /// `DIMSTYLE` — current dimension style handle. Defaults to null.
    pub dimstyle: HandleRef,
    /// `CMLSTYLE` — current multiline style handle. Defaults to null.
    pub cmlstyle: HandleRef,
}

impl Default for HeaderVars {
    fn default() -> Self {
        Self {
            required_versions: 0,
            insbase: [0.0; 3],
            extmin: [0.0; 3],
            extmax: [0.0; 3],
            limmin: [0.0; 2],
            limmax: [12.0, 9.0],
            ltscale: 1.0,
            textsize: 0.2,
            handseed: HandleRef {
                code: 0,
                value: 0x100,
            },
            clayer: HandleRef::default(),
            textstyle: HandleRef::default(),
            celtype: HandleRef::default(),
            cecolor: Color::ByLayer,
            dimstyle: HandleRef::default(),
            cmlstyle: HandleRef::default(),
        }
    }
}

/// Maintenance version supplied with the version tag. Mirrors
/// `dwg->header.maint_version` in LibreDWG. Used to gate two-field
/// extensions in R2010/R2013/R2018:
/// * R2010 / R2013 with `maint > 3` ⇒ split `bitsize_hi` field
/// * R2018+ ⇒ always split (regardless of maint)
///
/// For files we author, [`Version::maintenance_release`] returns the
/// stable LibreDWG-matched value (4 for R2010, 9 for R2013, 1 for
/// R2018), all of which trigger the split.
#[derive(Debug, Clone, Copy)]
pub struct MaintVersion(pub u8);

/// Encode the AcDb:Header section body in LibreDWG-conformant format.
///
/// Layout: RL size + bit-packed variables + CRC, *without* the 16-byte
/// sentinels. Works for any modern (R14+) DWG version.
pub fn encode_body(version: Version, maint: MaintVersion, vars: &HeaderVars) -> DwgResult<Vec<u8>> {
    // Build the three sub-streams into independent BitWriters. After
    // every field is emitted we have:
    //   sec_w → numeric/forward fields (main bit stream)
    //   str_w → wide-string fields (only for R2007+; otherwise empty)
    //   hdl_w → handle fields (R2007+; otherwise everything in sec_w)
    let mut sec_w = BitWriter::with_capacity(2048);
    let mut hdl_w = BitWriter::new();
    let mut str_w = BitWriter::new();
    emit_fields(version, vars, &mut sec_w, &mut hdl_w, &mut str_w)?;

    let main_bits = sec_w.bit_position();
    let hdl_bits = hdl_w.bit_position();
    let str_bits = str_w.bit_position();
    let sec_bytes = sec_w.into_bytes();
    let hdl_bytes = hdl_w.into_bytes();
    let str_bytes = str_w.into_bytes();

    // Assemble the final body bit stream.
    let mut body = BitWriter::with_capacity(2048);

    // 1) RL placeholder for size_in_bytes (patched at the end).
    let size_field_bit_pos = body.bit_position();
    body.write_rl(0)?;

    // For R2007+ we additionally write bitsize (and optionally
    // bitsize_hi for R2010+ with maint>3 or for R2018+).
    let split_streams = version.uses_utf16_strings();
    let bitsize_hi_emit = split_streams
        && (version >= Version::R2018 || (version >= Version::R2010 && u32::from(maint.0) > 3));
    if bitsize_hi_emit {
        // bitsize_hi placeholder (zero — we never need >2^32 bits).
        body.write_rl(0)?;
    }
    let bitsize_field_bit_pos = body.bit_position();
    if split_streams {
        body.write_rl(0)?;
    }

    body.append_bits_from(&sec_bytes, main_bits)?;

    // R2007+ string sub-stream: data_size bits of strings, then a
    // 16-bit RS data_size field, then a 1-bit endbit=1. The decoder
    // walks backward from `bitsize + 159` (R2007/R2010 maint<3) or
    // `bitsize + 191` (R2010 maint>3 / R2018) to find the endbit.
    if split_streams {
        // Extended size: if data_size > 0x7fff we need a second RS for
        // the high bits. For our writer the strings are short enough
        // that data_size fits in 15 bits; we surface an error rather
        // than silently truncating via `as u16`. If a future variable
        // bumps us over the threshold this path must learn the
        // hi_size encoding from `section_string_stream` in LibreDWG.
        if str_bits > 0x7fff {
            return Err(DwgError::InternalInvariant(format!(
                "header-vars string stream exceeded 0x7fff bits ({str_bits}); \
                 the extended-size encoding (high RS half) is not yet \
                 implemented -- either shorten the header-vars strings or \
                 add the hi_size emit/decode path"
            )));
        }
        body.append_bits_from(&str_bytes, str_bits)?;
        body.write_rs(str_bits as u16)?;
        body.write_b(true)?; // endbit
    }

    // Patch bitsize. The LibreDWG decoder computes
    //   endbits = (post-size-RL position) + bitsize          (R2007 / R2010 maint<3)
    //   endbits = (post-bitsize_hi-RL position) + bitsize    (R2010 maint>3 / R2018)
    // where `endbits` is the bit position IMMEDIATELY AFTER the
    // string-stream endbit (i.e., where the handle stream begins).
    // Equivalently, in both layouts the bitsize RL field itself is
    // included in the bitsize count, so:
    //   bitsize = (post-endbit position) - (position OF the bitsize RL field)
    // See `read_2004_section_header` in libredwg/src/decode.c:2330
    // and the symmetric calculation in `section_string_stream` at
    // libredwg/src/decode_r2007.c:1398 (`start = bitsize + 159|191`).
    let region_end = body.bit_position();
    let bitsize = region_end - bitsize_field_bit_pos;
    if split_streams {
        body.patch_rl_at(bitsize_field_bit_pos, bitsize as u32)?;
    }

    // For R2007+ append the handle stream after the bitsize region.
    if split_streams {
        body.append_bits_from(&hdl_bytes, hdl_bits)?;
    }

    // Align to byte boundary for the CRC.
    body.align_to_byte();

    let pre_crc_bytes = body.into_bytes();
    // pre_crc_bytes already contains the size field at offset 0; patch
    // its RL value to the body-length-in-bytes minus the size field.
    let size_field_byte_pos = (size_field_bit_pos / 8) as usize;
    let body_len_bytes = pre_crc_bytes.len() - size_field_byte_pos - 4;
    let mut final_bytes = pre_crc_bytes;
    final_bytes[size_field_byte_pos..size_field_byte_pos + 4]
        .copy_from_slice(&(body_len_bytes as u32).to_le_bytes());

    // CRC-X25 over the entire body (including RL size).
    let crc = crc_x25(0xc0c1, &final_bytes);
    final_bytes.extend_from_slice(&crc.to_le_bytes());
    Ok(final_bytes)
}

#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "line-for-line port of LibreDWG's `header_variables.spec` (700+ lines). \
              Mechanical decomposition into helpers would obscure the version-gating \
              correspondence with the upstream source-of-truth."
)]
fn emit_fields(
    version: Version,
    vars: &HeaderVars,
    sec: &mut BitWriter,
    hdl: &mut BitWriter,
    str_w: &mut BitWriter,
) -> DwgResult<()> {
    // Cached version predicates — these read like the SINCE/VERSIONS
    // macros in `header_variables.spec`.
    let r2000_plus = version >= Version::R2000;
    let r2004_plus = version >= Version::R2004;
    let r2007_plus = version >= Version::R2007;
    let r2010_plus = version >= Version::R2010;
    let r2013_plus = version >= Version::R2013;
    let pre_r2007 = !r2007_plus;
    let r13_to_r14 = version <= Version::R14; // VERSIONS(R_13b1, R_14)
    let r13_to_r2000 = version <= Version::R2000; // VERSIONS(R_13b1, R_2000)
    let r13_to_r2004 = version <= Version::R2004; // VERSIONS(R_13b1, R_2004) — controls TV unit names

    // Helper to write a FIELD_HANDLE to the right stream.
    let write_handle = |sec: &mut BitWriter, hdl: &mut BitWriter, h: HandleRef| -> DwgResult<()> {
        if r2007_plus {
            hdl.write_h(h)
        } else {
            sec.write_h(h)
        }
    };

    // === REQUIREDVERSIONS (R2013+) ===
    if r2013_plus {
        sec.write_bll(vars.required_versions)?;
    }

    // === SINCE(R_13b1): unit*_ratio (4 × BD) ===
    sec.write_bd(412_148_564_080.0)?; // unit1_ratio default per spec
    sec.write_bd(1.0)?; // unit2_ratio
    sec.write_bd(1.0)?; // unit3_ratio
    sec.write_bd(1.0)?; // unit4_ratio

    // === VERSIONS(R_13b1, R_2004): unit1..unit4 names as TV ===
    if r13_to_r2004 {
        sec.write_tv("m")?;
        sec.write_tv("")?;
        sec.write_tv("")?;
        sec.write_tv("")?;
    }

    // === SINCE(R_13b1): unknown_8 BL=24 default, unknown_9 BL ===
    sec.write_bl(24)?;
    sec.write_bl(0)?;

    // === VERSIONS(R_13b1, R_14): unknown_10 BS ===
    if r13_to_r14 {
        sec.write_bs(0)?;
    }

    // === VERSIONS(R_13b1, R_2000): VX_TABLE_RECORD handle ===
    if r13_to_r2000 {
        write_handle(sec, hdl, HandleRef::default())?;
    }

    // === SINCE(R_13b1): DIMASO, DIMSHO (B) ===
    sec.write_b(true)?; // DIMASO default = 1
    sec.write_b(true)?; // DIMSHO default = 1

    // === VERSIONS(R_13b1, R_14): DIMSAV B ===
    if r13_to_r14 {
        sec.write_b(false)?;
    }

    // === SINCE(R_13b1): PLINEGEN/ORTHOMODE/REGENMODE/FILLMODE/QTEXTMODE/PSLTSCALE/LIMCHECK (B) ===
    sec.write_b(false)?; // PLINEGEN
    sec.write_b(false)?; // ORTHOMODE
    sec.write_b(true)?; // REGENMODE
    sec.write_b(true)?; // FILLMODE
    sec.write_b(false)?; // QTEXTMODE
    sec.write_b(true)?; // PSLTSCALE
    sec.write_b(false)?; // LIMCHECK

    if r13_to_r14 {
        sec.write_b(false)?; // BLIPMODE
    }
    if r2004_plus {
        sec.write_b(false)?; // unknown_11
    }

    // USRTIMER, SKPOLY, ANGDIR, SPLFRAME — all B
    sec.write_b(true)?; // USRTIMER
    sec.write_b(false)?; // SKPOLY
    sec.write_b(false)?; // ANGDIR
    sec.write_b(false)?; // SPLFRAME

    if r13_to_r14 {
        sec.write_b(true)?; // ATTREQ default = 1
        sec.write_b(true)?; // ATTDIA default = 1
    }

    sec.write_b(false)?; // MIRRTEXT
    sec.write_b(true)?; // WORLDVIEW default = 1

    if r13_to_r14 {
        sec.write_b(false)?; // WIREFRAME
    }

    sec.write_b(true)?; // TILEMODE default = 1
    sec.write_b(false)?; // PLIMCHECK
    sec.write_b(true)?; // VISRETAIN default = 1

    if r13_to_r14 {
        sec.write_b(false)?; // DELOBJ
    }

    sec.write_b(false)?; // DISPSILH
    sec.write_b(false)?; // PELLIPSE

    // PROXYGRAPHICS BS (1)
    sec.write_bs(1)?;

    if r13_to_r14 {
        sec.write_bs(2)?; // DRAGMODE default 2
    }

    // SINCE(R_13b1): TREEDEPTH BSd default 3020
    sec.write_bs(3020)?;

    sec.write_bs(1)?; // LUNITS — assumed default 1 (scientific) per ODA
    sec.write_bs(4)?; // LUPREC
    sec.write_bs(0)?; // AUNITS
    sec.write_bs(0)?; // AUPREC

    if r13_to_r14 {
        sec.write_bs(0)?; // OSMODE
    }
    sec.write_bs(0)?; // ATTMODE
    if r13_to_r14 {
        sec.write_bs(0)?; // COORDS
    }
    sec.write_bs(0)?; // PDMODE
    if r13_to_r14 {
        sec.write_bs(0)?; // PICKSTYLE
    }

    if r2004_plus {
        sec.write_bl(0)?; // unknown_12
        sec.write_bl(0)?; // unknown_13
        sec.write_bl(0)?; // unknown_14
    }

    // USERI1..USERI5 BSd
    for _ in 0..5 {
        sec.write_bs(0)?;
    }

    // SPLINESEGS .. TEXTQLTY (15 × BS)
    sec.write_bs(8)?; // SPLINESEGS default 8
    sec.write_bs(6)?; // SURFU
    sec.write_bs(6)?; // SURFV
    sec.write_bs(6)?; // SURFTYPE
    sec.write_bs(6)?; // SURFTAB1
    sec.write_bs(6)?; // SURFTAB2
    sec.write_bs(6)?; // SPLINETYPE
    sec.write_bs(3)?; // SHADEDGE default 3
    sec.write_bs(70)?; // SHADEDIF default 70
    sec.write_bs(0)?; // UNITMODE
    sec.write_bs(64)?; // MAXACTVP default 64
    sec.write_bs(4)?; // ISOLINES
    sec.write_bs(0)?; // CMLJUST
    sec.write_bs(50)?; // TEXTQLTY

    // 23 × BD: LTSCALE, TEXTSIZE, ...
    sec.write_bd(vars.ltscale)?;
    sec.write_bd(vars.textsize)?;
    sec.write_bd(0.05)?; // TRACEWID
    sec.write_bd(0.1)?; // SKETCHINC
    sec.write_bd(0.5)?; // FILLETRAD
    sec.write_bd(0.0)?; // THICKNESS
    sec.write_bd(0.0)?; // ANGBASE
    sec.write_bd(0.0)?; // PDSIZE
    sec.write_bd(0.0)?; // PLINEWID
    sec.write_bd(0.0)?; // USERR1
    sec.write_bd(0.0)?; // USERR2
    sec.write_bd(0.0)?; // USERR3
    sec.write_bd(0.0)?; // USERR4
    sec.write_bd(0.0)?; // USERR5
    sec.write_bd(0.5)?; // CHAMFERA
    sec.write_bd(0.5)?; // CHAMFERB
    sec.write_bd(0.0)?; // CHAMFERC
    sec.write_bd(0.0)?; // CHAMFERD
    sec.write_bd(0.5)?; // FACETRES
    sec.write_bd(1.0)?; // CMLSCALE
    sec.write_bd(1.0)?; // CELTSCALE

    if pre_r2007 {
        sec.write_tv("")?; // MENU
    }

    // TIMEBLL TDUCREATE/TDUUPDATE — each two BL (days + ms)
    for _ in 0..4 {
        sec.write_bl(0)?;
    }

    if r2004_plus {
        sec.write_bl(0)?; // unknown_15
        sec.write_bl(0)?; // unknown_16
        sec.write_bl(0)?; // unknown_17
    }

    // TIMEBLL TDINDWG, TDUSRTIMER
    for _ in 0..4 {
        sec.write_bl(0)?;
    }

    sec.write_cmc_v(version, &vars.cecolor)?;

    // FIELD_DATAHANDLE: same on-wire shape as HANDLE.
    sec.write_h(vars.handseed)?;
    write_handle(sec, hdl, vars.clayer)?;
    write_handle(sec, hdl, vars.textstyle)?;
    write_handle(sec, hdl, vars.celtype)?;

    if r2007_plus {
        write_handle(sec, hdl, HandleRef::default())?; // CMATERIAL
    }

    write_handle(sec, hdl, vars.dimstyle)?;
    write_handle(sec, hdl, vars.cmlstyle)?;

    if r2000_plus {
        sec.write_bd(1.0)?; // PSVPSCALE
    }

    // SINCE(R_13b1) PSPACE block
    sec.write_3bd([0.0; 3])?; // PINSBASE
    sec.write_3bd([0.0; 3])?; // PEXTMIN
    sec.write_3bd([0.0; 3])?; // PEXTMAX
    sec.write_2rd([0.0; 2])?; // PLIMMIN
    sec.write_2rd([12.0, 9.0])?; // PLIMMAX
    sec.write_bd(0.0)?; // PELEVATION
    sec.write_3bd([0.0; 3])?; // PUCSORG
    sec.write_3bd([1.0, 0.0, 0.0])?; // PUCSXDIR
    sec.write_3bd([0.0, 1.0, 0.0])?; // PUCSYDIR
    write_handle(sec, hdl, HandleRef::default())?; // PUCSNAME

    if r2000_plus {
        write_handle(sec, hdl, HandleRef::default())?; // PUCSORTHOREF
        sec.write_bs(0)?; // PUCSORTHOVIEW
        write_handle(sec, hdl, HandleRef::default())?; // PUCSBASE
        for _ in 0..6 {
            sec.write_3bd([0.0; 3])?; // PUCSORGTOP..BACK
        }
    }

    // Drawing-extent block
    sec.write_3bd(vars.insbase)?;
    sec.write_3bd(vars.extmin)?;
    sec.write_3bd(vars.extmax)?;
    sec.write_2rd(vars.limmin)?;
    sec.write_2rd(vars.limmax)?;
    sec.write_bd(0.0)?; // ELEVATION
    sec.write_3bd([0.0; 3])?; // UCSORG
    sec.write_3bd([1.0, 0.0, 0.0])?; // UCSXDIR
    sec.write_3bd([0.0, 1.0, 0.0])?; // UCSYDIR
    write_handle(sec, hdl, HandleRef::default())?; // UCSNAME

    if r2000_plus {
        write_handle(sec, hdl, HandleRef::default())?; // UCSORTHOREF
        sec.write_bs(0)?; // UCSORTHOVIEW
        write_handle(sec, hdl, HandleRef::default())?; // UCSBASE
        for _ in 0..6 {
            sec.write_3bd([0.0; 3])?; // UCSORGTOP..BACK
        }
        if pre_r2007 {
            sec.write_tv("")?; // DIMPOST
            sec.write_tv("")?; // DIMAPOST
        }
    }

    if r13_to_r14 {
        // R13/R14 DIM block — 14 × B + 9 × RC + 6 × BS + 1 HANDLE
        for _ in 0..11 {
            sec.write_b(false)?; // DIMTOL..DIMSOXD
        }
        sec.write_rc(0)?; // DIMALTD (CAST RC, BS)
        sec.write_rc(0)?; // DIMZIN
        sec.write_b(false)?; // DIMSD1
        sec.write_b(false)?; // DIMSD2
        sec.write_rc(0)?; // DIMTOLJ
        sec.write_rc(0)?; // DIMJUST
        sec.write_rc(0)?; // DIMFIT
        sec.write_b(false)?; // DIMUPT
        sec.write_rc(0)?; // DIMTZIN
        sec.write_rc(0)?; // DIMALTZ
        sec.write_rc(0)?; // DIMALTTZ
        sec.write_rc(0)?; // DIMTAD
        sec.write_bs(0)?; // DIMUNIT
        sec.write_bs(0)?; // DIMAUNIT
        sec.write_bs(0)?; // DIMDEC
        sec.write_bs(0)?; // DIMTDEC
        sec.write_bs(0)?; // DIMALTU
        sec.write_bs(0)?; // DIMALTTD
        write_handle(sec, hdl, HandleRef::default())?; // DIMTXSTY
    }

    // DIMSCALE..DIMTM (9 × BD)
    sec.write_bd(1.0)?; // DIMSCALE
    sec.write_bd(0.18)?; // DIMASZ
    sec.write_bd(0.0625)?; // DIMEXO
    sec.write_bd(0.38)?; // DIMDLI
    sec.write_bd(0.18)?; // DIMEXE
    sec.write_bd(0.0)?; // DIMRND
    sec.write_bd(0.0)?; // DIMDLE
    sec.write_bd(0.0)?; // DIMTP
    sec.write_bd(0.0)?; // DIMTM

    if r2007_plus {
        sec.write_bd(1.0)?; // DIMFXL
        sec.write_bd(0.0)?; // DIMJOGANG
        sec.write_bs(0)?; // DIMTFILL
        sec.write_cmc_v(version, &Color::ByLayer)?; // DIMTFILLCLR
    }

    if r2000_plus {
        // R2000+ DIMTOL/DIMLIM/...
        sec.write_b(false)?; // DIMTOL
        sec.write_b(false)?; // DIMLIM
        sec.write_b(true)?; // DIMTIH default 1
        sec.write_b(true)?; // DIMTOH default 1
        sec.write_b(false)?; // DIMSE1
        sec.write_b(false)?; // DIMSE2
        sec.write_bs(0)?; // DIMTAD
        sec.write_bs(0)?; // DIMZIN
        sec.write_bs(0)?; // DIMAZIN
    }
    if r2007_plus {
        sec.write_bs(0)?; // DIMARCSYM
    }

    sec.write_bd(0.18)?; // DIMTXT
    sec.write_bd(0.09)?; // DIMCEN
    sec.write_bd(0.0)?; // DIMTSZ
    sec.write_bd(25.4)?; // DIMALTF
    sec.write_bd(1.0)?; // DIMLFAC
    sec.write_bd(0.0)?; // DIMTVP
    sec.write_bd(1.0)?; // DIMTFAC
    sec.write_bd(0.09)?; // DIMGAP

    // IF_FREE_OR_VERSIONS(R_13b1, R_14) — same as VERSIONS(R_13b1, R_14)
    // for our writer (FREE branch is decoder-only).
    if r13_to_r14 {
        sec.write_tv("")?; // DIMPOST
        sec.write_tv("")?; // DIMAPOST
        sec.write_tv("")?; // DIMBLK_T
        sec.write_tv("")?; // DIMBLK1_T
        sec.write_tv("")?; // DIMBLK2_T
    }

    if r2000_plus {
        sec.write_bd(0.0)?; // DIMALTRND
        sec.write_b(false)?; // DIMALT
        sec.write_bs(2)?; // DIMALTD
        sec.write_b(false)?; // DIMTOFL
        sec.write_b(true)?; // DIMSAH
        sec.write_b(false)?; // DIMTIX
        sec.write_b(false)?; // DIMSOXD
    }

    sec.write_cmc_v(version, &Color::ByBlock)?; // DIMCLRD
    sec.write_cmc_v(version, &Color::ByBlock)?; // DIMCLRE
    sec.write_cmc_v(version, &Color::ByBlock)?; // DIMCLRT

    if r2000_plus {
        sec.write_bs(0)?; // DIMADEC
        sec.write_bs(4)?; // DIMDEC
        sec.write_bs(4)?; // DIMTDEC
        sec.write_bs(2)?; // DIMALTU
        sec.write_bs(2)?; // DIMALTTD
        sec.write_bs(0)?; // DIMAUNIT
        sec.write_bs(0)?; // DIMFRAC
        sec.write_bs(2)?; // DIMLUNIT
        sec.write_bs(0)?; // DIMDSEP
        sec.write_bs(0)?; // DIMTMOVE
        sec.write_bs(1)?; // DIMJUST
        sec.write_b(false)?; // DIMSD1
        sec.write_b(false)?; // DIMSD2
        sec.write_bs(1)?; // DIMTOLJ
        sec.write_bs(0)?; // DIMTZIN
        sec.write_bs(0)?; // DIMALTZ
        sec.write_bs(0)?; // DIMALTTZ
        sec.write_b(false)?; // DIMUPT
        sec.write_bs(3)?; // DIMATFIT
    }

    if r2007_plus {
        sec.write_b(false)?; // DIMFXLON
    }

    if r2010_plus {
        sec.write_b(false)?; // DIMTXTDIRECTION
        sec.write_bd(100.0)?; // DIMALTMZF
        sec.write_bd(100.0)?; // DIMMZF
    }

    if r2000_plus {
        write_handle(sec, hdl, HandleRef::default())?; // DIMTXSTY
        write_handle(sec, hdl, HandleRef::default())?; // DIMLDRBLK
        write_handle(sec, hdl, HandleRef::default())?; // DIMBLK
        write_handle(sec, hdl, HandleRef::default())?; // DIMBLK1
        write_handle(sec, hdl, HandleRef::default())?; // DIMBLK2
    }

    if r2007_plus {
        write_handle(sec, hdl, HandleRef::default())?; // DIMLTYPE
        write_handle(sec, hdl, HandleRef::default())?; // DIMLTEX1
        write_handle(sec, hdl, HandleRef::default())?; // DIMLTEX2
    }

    if r2000_plus {
        sec.write_bs(-2)?; // DIMLWD (-2 = ByLayer)
        sec.write_bs(-2)?; // DIMLWE
    }

    // Control-object handles
    write_handle(sec, hdl, HandleRef::default())?; // BLOCK_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // LAYER_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // STYLE_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // LTYPE_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // VIEW_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // UCS_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // VPORT_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // APPID_CONTROL_OBJECT
    write_handle(sec, hdl, HandleRef::default())?; // DIMSTYLE_CONTROL_OBJECT

    if r13_to_r2000 {
        write_handle(sec, hdl, HandleRef::default())?; // VX_CONTROL_OBJECT
    }

    write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_ACAD_GROUP
    write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_ACAD_MLINESTYLE
    write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_NAMED_OBJECT

    if r2000_plus {
        sec.write_bs(1)?; // TSTACKALIGN default 1
        sec.write_bs(70)?; // TSTACKSIZE default 70
        if pre_r2007 {
            sec.write_tv("")?; // HYPERLINKBASE
            sec.write_tv("")?; // STYLESHEET
        }
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_LAYOUT
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_PLOTSETTINGS
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_PLOTSTYLENAME
    }

    if r2004_plus {
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_MATERIAL
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_COLOR
    }
    if r2007_plus {
        write_handle(sec, hdl, HandleRef::default())?; // DICTIONARY_VISUALSTYLE
    }
    if r2013_plus {
        write_handle(sec, hdl, HandleRef::default())?; // unknown_20
    }

    if r2000_plus {
        // FLAGS BLx — packs CELWEIGHT/ENDCAPS/JOINSTYLE/LWDISPLAY/...
        // Compose flags = CELWEIGHT_ByLayer (0x1f) | LWDISPLAY=on (0x200 cleared) |
        //                 XEDIT=on (0x400 cleared)
        // The decoder reads CELWEIGHT = flags & 0x1f; values 0..0x1c map to
        // weights, 0x1d/0x1e/0x1f map to ByBlock/ByLayer/default. We emit
        // 0x1f (ByLayer) and clear the higher inverted bits.
        sec.write_bl(0x1f)?;
        sec.write_bs(1)?; // INSUNITS default 1 (inches)
        sec.write_bs(0)?; // CEPSNTYPE
                          // CEPSNTYPE==3 ⇒ CPSNID handle — not emitted since CEPSNTYPE=0.
        if pre_r2007 {
            sec.write_tv("")?; // FINGERPRINTGUID
            sec.write_tv("")?; // VERSIONGUID
        }
    }

    if r2004_plus {
        sec.write_rc(0)?; // SORTENTS
        sec.write_rc(0)?; // INDEXCTL
        sec.write_rc(0)?; // HIDETEXT
        sec.write_rc(2)?; // XCLIPFRAME default 2
        sec.write_rc(1)?; // DIMASSOC default 1
        sec.write_rc(0)?; // HALOGAP
        sec.write_bs(257)?; // OBSCOLOR — 257 = ByEntity
        sec.write_bs(257)?; // INTERSECTIONCOLOR
        sec.write_rc(0)?; // OBSLTYPE
        sec.write_rc(0)?; // INTERSECTIONDISPLAY
        if pre_r2007 {
            sec.write_tv("")?; // PROJECTNAME
        }
    }

    write_handle(sec, hdl, HandleRef::default())?; // BLOCK_RECORD_PSPACE
    write_handle(sec, hdl, HandleRef::default())?; // BLOCK_RECORD_MSPACE
    write_handle(sec, hdl, HandleRef::default())?; // LTYPE_BYLAYER
    write_handle(sec, hdl, HandleRef::default())?; // LTYPE_BYBLOCK
    write_handle(sec, hdl, HandleRef::default())?; // LTYPE_CONTINUOUS

    if r2007_plus {
        sec.write_b(false)?; // CAMERADISPLAY
        sec.write_bl(0)?; // unknown_21
        sec.write_bl(0)?; // unknown_22
        sec.write_bd(0.0)?; // unknown_23
        sec.write_bd(2.0)?; // STEPSPERSEC
        sec.write_bd(50.0)?; // STEPSIZE
        sec.write_bd(2.0)?; // _3DDWFPREC
        sec.write_bd(50.0)?; // LENSLENGTH
        sec.write_bd(0.0)?; // CAMERAHEIGHT
        sec.write_rc(1)?; // SOLIDHIST default 1
        sec.write_rc(1)?; // SHOWHIST default 1
        sec.write_bd(5.0)?; // PSOLWIDTH
        sec.write_bd(80.0)?; // PSOLHEIGHT
        sec.write_bd(std::f64::consts::FRAC_PI_2)?; // LOFTANG1
        sec.write_bd(std::f64::consts::FRAC_PI_2)?; // LOFTANG2
        sec.write_bd(0.0)?; // LOFTMAG1
        sec.write_bd(0.0)?; // LOFTMAG2
        sec.write_bs(7)?; // LOFTPARAM
        sec.write_rc(1)?; // LOFTNORMALS
        sec.write_bd(1.0)?; // LATITUDE
        sec.write_bd(1.0)?; // LONGITUDE
        sec.write_bd(0.0)?; // NORTHDIRECTION
        sec.write_bl(-8000)?; // TIMEZONE (BLd: signed)
        sec.write_rc(1)?; // LIGHTGLYPHDISPLAY
        sec.write_rc(1)?; // TILEMODELIGHTSYNCH
        sec.write_rc(2)?; // DWFFRAME default 2
        sec.write_rc(0)?; // DGNFRAME
        sec.write_b(true)?; // REALWORLDSCALE
        sec.write_cmc_v(version, &Color::Index(7))?; // INTERFERECOLOR — by name
        write_handle(sec, hdl, HandleRef::default())?; // INTERFEREOBJVS
        write_handle(sec, hdl, HandleRef::default())?; // INTERFEREVPVS
        write_handle(sec, hdl, HandleRef::default())?; // DRAGVS
        sec.write_rc(0)?; // CSHADOW
        sec.write_bd(0.0)?; // SHADOWPLANELOCATION
    }

    // SINCE(R_14): four unknown BS pads
    sec.write_bs(0)?; // unknown_54
    sec.write_bs(0)?; // unknown_55
    sec.write_bs(0)?; // unknown_56
    sec.write_bs(0)?; // unknown_57

    if r2007_plus {
        // SECTION_STRING_STREAM — emit all wide strings into str_w in order.
        // For default-empty values each `write_t("")` emits a 2-bit BS=0
        // length prefix (no payload).
        str_w.write_t("m")?; // unit1_name
        str_w.write_t("")?; // unit2_name
        str_w.write_t("")?; // unit3_name
        str_w.write_t("")?; // unit4_name
        str_w.write_t("")?; // MENU
        str_w.write_t("")?; // DIMPOST
        str_w.write_t("")?; // DIMAPOST
        if r2010_plus {
            str_w.write_t("")?; // DIMALTMZS
            str_w.write_t("")?; // DIMMZS
        }
        str_w.write_t("")?; // HYPERLINKBASE
        str_w.write_t("")?; // STYLESHEET
        str_w.write_t("")?; // FINGERPRINTGUID
        str_w.write_t("")?; // VERSIONGUID
        str_w.write_t("")?; // PROJECTNAME
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_emitted_for_every_modern_version() {
        for v in [
            Version::R14,
            Version::R2000,
            Version::R2004,
            Version::R2007,
            Version::R2010,
            Version::R2013,
            Version::R2018,
        ] {
            let body = encode_body(
                v,
                MaintVersion(v.maintenance_release()),
                &HeaderVars::default(),
            )
            .unwrap_or_else(|e| panic!("encode_body failed for {v:?}: {e}"));
            // Body must have at least: RL size (4) + some payload + CRC (2).
            assert!(
                body.len() > 6,
                "{v:?} body too small ({} bytes)",
                body.len()
            );
            // The RL size prefix at offset 0 must equal body.len() - 4 - 2
            // (i.e. payload bytes between the size field and the CRC).
            let size_field = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            assert_eq!(
                size_field,
                body.len() - 4 - 2,
                "{v:?} RL size mismatch: field={} body={} expected={}",
                size_field,
                body.len(),
                body.len() - 4 - 2,
            );
            // The trailing CRC must validate against the body up to that point.
            let crc_off = body.len() - 2;
            let stored = u16::from_le_bytes([body[crc_off], body[crc_off + 1]]);
            let computed = crc_x25(0xc0c1, &body[..crc_off]);
            assert_eq!(stored, computed, "{v:?} CRC mismatch");
        }
    }

    #[test]
    fn r2007_body_uses_split_streams_so_bitsize_field_is_present() {
        // For R2007+ the RL right after the size field is bitsize. For
        // R13/R14/R2000/R2004 there's no bitsize field — the next bits
        // are the first variable.
        let body = encode_body(Version::R2007, MaintVersion(0), &HeaderVars::default()).unwrap();
        // size RL at [0..4], bitsize RL at [4..8].
        let bitsize = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        // bitsize should be strictly less than total body bits.
        let total_bits = (body.len() as u32 - 2/* CRC */) * 8;
        assert!(
            bitsize > 0 && bitsize < total_bits,
            "R2007 bitsize {bitsize} not in (0, {total_bits})"
        );
    }

    #[test]
    fn r2010_with_high_maint_emits_bitsize_hi() {
        // R2010 with maint > 3 ⇒ bitsize_hi field appears between size
        // and bitsize. Total body length should be 4 bytes larger than
        // R2010 with maint = 0.
        let body_hi = encode_body(Version::R2010, MaintVersion(4), &HeaderVars::default()).unwrap();
        let body_lo = encode_body(Version::R2010, MaintVersion(0), &HeaderVars::default()).unwrap();
        assert_eq!(
            body_hi.len(),
            body_lo.len() + 4,
            "R2010 maint=4 body should be 4 bytes longer than maint=0",
        );
        // bitsize_hi is zero in our writer.
        assert_eq!(&body_hi[4..8], &[0, 0, 0, 0], "bitsize_hi must be 0");
    }

    #[test]
    fn r2004_body_has_no_bitsize_field() {
        // For pre-R2007, all fields go into a single stream and there
        // is no bitsize header. The bytes right after the size field
        // are the first BD field (unit1_ratio = 412148564080.0 raw).
        let body = encode_body(Version::R2004, MaintVersion(0), &HeaderVars::default()).unwrap();
        // The next byte after size RL must be the BB prefix of unit1_ratio
        // (a non-default BD ⇒ BB=00, so the first 2 bits are 00). Read 1 byte
        // and inspect its high 2 bits.
        let first_payload = body[4];
        assert_eq!(first_payload >> 6, 0b00, "R2004 first BD prefix not 00");
    }
}
