//! AC1009 (AutoCAD R12) assembler / disassembler.
//!
//! Produces and consumes the wire format that LibreDWG calls
//! `PRE(R_13b1)`. The layout of the output is fixed and mirrors the
//! `encode.c::dwg_encode` codepath at lines 2956-3187 of LibreDWG.
//!
//! High-level shape (offsets in the leading file header point at
//! every variable-position landmark):
//!
//! ```text
//! 0x00 ─── R12 file header                                 [44 B]
//!         (version + numentity_sections + num_sections +
//!          numheader_vars + dwg_version + section locator)
//! 0x2C ─── Five contiguous section descriptors             [50 B]
//!         (BLOCK, LAYER, STYLE, LTYPE, VIEW; each 10 B)
//! 0x5E ─── Header variables (1631 B)                       — embeds
//!         the other five section descriptors (UCS, VPORT,
//!         APPID, DIMSTYLE, VX) at offsets dictated by
//!         `header_variables_r11.spec` lines 269..376.
//! 0x6BD ── 2-byte CRC placeholder (patched at end)         — sits
//!         exactly 18 bytes before `entities_start`.
//! 0x6BF ── DWG_SENTINEL_R11_ENTITIES_BEGIN                 [16 B]
//! 0x6CF ── entities payload                                 — caller-
//!         supplied bytes; pre-encoded AC1009 entity records.
//! ────── DWG_SENTINEL_R11_ENTITIES_END                     [16 B]
//! ────── Ten empty table sections (32 B each)              [320 B]
//!         BLOCK, LAYER, STYLE, LTYPE, VIEW, UCS, VPORT,
//!         APPID, DIMSTYLE, VX (each: BEGIN + END sentinel,
//!         no records because every descriptor has number=0).
//! ────── DWG_SENTINEL_R11_BLOCK_ENTITIES_BEGIN             [16 B]
//! ────── block-entities payload (typically empty)
//! ────── DWG_SENTINEL_R11_BLOCK_ENTITIES_END               [16 B]
//! ────── DWG_SENTINEL_R11_EXTRA_ENTITIES_BEGIN             [16 B]
//! ────── extra-entities payload (typically empty)
//! ────── DWG_SENTINEL_R11_EXTRA_ENTITIES_END               [16 B]
//! ────── R11 aux header (sentinel-framed, 170 B total)
//!         (BEGIN sentinel + 138-byte payload + END sentinel)
//! eof ─── (no further bytes)
//! ```
//!
//! Two structural numbers are patched after the trailing sections
//! have been laid out:
//!
//! 1. The six section-locator offsets at `0x14..0x2C`
//!    (`entities_start`, `entities_end`, `blocks_start`, `blocks_size`,
//!    `extras_start`, `extras_size`).
//! 2. The 2-byte CRC at `entities_start - 18`, computed over the
//!    file's first `entities_start - 18` bytes.
//!
//! `blocks_size` is OR'd with `0x40000000` per
//! `encode.c:3107-3108` (true for every R11+ version, and our R12
//! emission counts as R_11 in LibreDWG's enum). `extras_size` is
//! OR'd with `0x80000000` per `encode.c:3122-3124`.

use crate::dwg::bits::crc::crc_x25;
use crate::dwg::error::{DwgError, DwgResult};
use crate::dwg::r12::spec::file_header::{
    patch_section_locator, R12FileHeader, R12SectionLocator, FILE_HEADER_LEN,
};
use crate::dwg::r12::spec::header_vars::{
    DecodedHeaderVars, R12HeaderVars, ENCODED_LEN as HEADER_VARS_LEN,
};
use crate::dwg::r12::spec::section_table::{
    R12SectionTables, SectionTableHeader, LEADING_SECTION_REGION_END, LEADING_SECTION_REGION_START,
};
use crate::dwg::r12::spec::sentinels::{
    self, Sentinel, APPID_BEGIN, APPID_END, AUXHEADER_BEGIN, AUXHEADER_END, BLOCK_BEGIN, BLOCK_END,
    BLOCK_ENTITIES_BEGIN, BLOCK_ENTITIES_END, DIMSTYLE_BEGIN, DIMSTYLE_END, ENTITIES_BEGIN,
    ENTITIES_END, EXTRA_ENTITIES_BEGIN, EXTRA_ENTITIES_END, LAYER_BEGIN, LAYER_END, LTYPE_BEGIN,
    LTYPE_END, STYLE_BEGIN, STYLE_END, UCS_BEGIN, UCS_END, VIEW_BEGIN, VIEW_END, VPORT_BEGIN,
    VPORT_END, VX_BEGIN, VX_END,
};

/// Length of one sentinel in bytes (always 16 for R11).
pub const SENTINEL_LEN: usize = sentinels::SENTINEL_LEN;

/// Width of the aux header *payload* (excluding the wrapping
/// `AUXHEADER_BEGIN` / `AUXHEADER_END` sentinels). Matches
/// `encode.c:1741` which sets `_obj->auxheader_size = 138`.
const AUX_HEADER_PAYLOAD_LEN: usize = 138;

/// Width of the auxiliary header section including both sentinels.
pub const AUX_HEADER_LEN: usize = AUX_HEADER_PAYLOAD_LEN + 2 * SENTINEL_LEN;

/// The number of empty per-table sections emitted between
/// `entities_end` and `blocks_start`. Always 10 for R12
/// (BLOCK/LAYER/STYLE/LTYPE/VIEW + UCS/VPORT/APPID/DIMSTYLE/VX).
pub const EMPTY_TABLE_COUNT: usize = 10;

/// Width of one empty per-table section (BEGIN sentinel + END
/// sentinel, no records).
pub const EMPTY_TABLE_LEN: usize = 2 * SENTINEL_LEN;

/// Width of the payload size field embedded in `blocks_size` /
/// `extras_size`. Per `decode_r11.c` LibreDWG strips the high byte of
/// these fields via `& 0xffffff` to recover the payload length, so the
/// AC1009 wire format effectively caps each region at 16 MB minus one.
/// The high-byte bits are reserved for the type flags
/// ([`BLOCKS_SIZE_FLAG`], [`EXTRAS_SIZE_FLAG`]).
pub const SIZE_FIELD_MASK: u32 = 0x00FF_FFFF;

/// Bit-30 flag OR'd into `blocks_size` per `encode.c:3107-3108`
/// whenever `version > R_2_22` (always true for R12). Without it,
/// LibreDWG reads `blocks_size` as zero and skips block parsing.
pub const BLOCKS_SIZE_FLAG: u32 = 0x4000_0000;

/// Bit-31 flag OR'd into `extras_size` per `encode.c:3122-3124`
/// (R12+).
pub const EXTRAS_SIZE_FLAG: u32 = 0x8000_0000;

/// Sentinel-pair sequence for the ten empty per-table sections.
/// The order matches `encode.c:3072-3089` exactly.
const EMPTY_TABLE_SENTINELS: [(Sentinel, Sentinel); EMPTY_TABLE_COUNT] = [
    (BLOCK_BEGIN, BLOCK_END),
    (LAYER_BEGIN, LAYER_END),
    (STYLE_BEGIN, STYLE_END),
    (LTYPE_BEGIN, LTYPE_END),
    (VIEW_BEGIN, VIEW_END),
    (UCS_BEGIN, UCS_END),
    (VPORT_BEGIN, VPORT_END),
    (APPID_BEGIN, APPID_END),
    (DIMSTYLE_BEGIN, DIMSTYLE_END),
    (VX_BEGIN, VX_END),
];

/// One R12 file's worth of input bytes. The caller supplies the
/// already-encoded entity / block / extras streams and the header
/// variables; the assembler does the framing, locator patching and
/// CRC.
#[derive(Debug, Clone, PartialEq)]
pub struct R12Assembly {
    /// Header variables. The aux header copies `HANDSEED` and
    /// `HANDLING` out of this for its own fields.
    pub header_vars: R12HeaderVars,
    /// Pre-encoded entity records (one fully-framed
    /// fixed-record-with-CRC per entity, in walk order). Goes into
    /// the `ENTITIES_BEGIN..ENTITIES_END` region.
    pub entities: Vec<u8>,
    /// Pre-encoded block-scoped entity records. Goes into the
    /// `BLOCK_ENTITIES_BEGIN..BLOCK_ENTITIES_END` region. For PR-F2
    /// scope the bridge always passes empty here; the assembler
    /// still emits the sentinel frame so the file is structurally
    /// complete.
    pub block_entities: Vec<u8>,
    /// Pre-encoded extra-entity records (DICTIONARYs, XRECORDs,
    /// etc.). Goes into the
    /// `EXTRA_ENTITIES_BEGIN..EXTRA_ENTITIES_END` region. PR-F2
    /// scope: always empty.
    pub extras: Vec<u8>,
}

impl R12Assembly {
    /// Build an empty (default) assembly. Useful for the "fresh
    /// canvas" path the LibreDWG oracle exercises.
    pub fn empty() -> Self {
        Self {
            header_vars: R12HeaderVars::default(),
            entities: Vec::new(),
            block_entities: Vec::new(),
            extras: Vec::new(),
        }
    }
}

/// Result of disassembling an AC1009 file image: the parsed
/// header_vars plus the three sentinel-framed entity regions plus
/// the ten section descriptors as they appeared on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct R12Disassembly {
    pub header_vars: R12HeaderVars,
    pub entities: Vec<u8>,
    pub block_entities: Vec<u8>,
    pub extras: Vec<u8>,
    pub section_tables: R12SectionTables,
    pub file_header: R12FileHeader,
}

/// Encode an [`R12Assembly`] as a complete AC1009 file image.
pub fn assemble(input: &R12Assembly) -> DwgResult<Vec<u8>> {
    let mut out = Vec::with_capacity(8 * 1024);

    // ------------------------------------------------------------
    // (1) Reserve the leading file header (44 bytes). We patch the
    //     section locator in later.
    // ------------------------------------------------------------
    let file_header = R12FileHeader::default();
    out.extend_from_slice(&file_header.encode());
    debug_assert_eq!(out.len(), FILE_HEADER_LEN);

    // ------------------------------------------------------------
    // (2) Reserve the leading five section descriptors (50 bytes).
    //     For PR-F2 scope every descriptor is empty (number=0,
    //     size=0, address=0) so the leading region is fifty zero
    //     bytes; we patch the bytes via the section-table encoder
    //     anyway so the format stays explicit.
    // ------------------------------------------------------------
    let section_tables = R12SectionTables::default();
    out.extend_from_slice(&section_tables.encode_leading_five());
    debug_assert_eq!(out.len(), LEADING_SECTION_REGION_END);

    // ------------------------------------------------------------
    // (3) Header variables (1631 bytes). This embeds the other five
    //     section descriptors (UCS, VPORT, APPID, DIMSTYLE, VX) at
    //     fixed in-block offsets.
    // ------------------------------------------------------------
    let hv_bytes = input
        .header_vars
        .encode_with_section_tables(&section_tables);
    if hv_bytes.len() != HEADER_VARS_LEN {
        return Err(DwgError::InternalInvariant(format!(
            "R12 header_vars expected {} bytes, encoder produced {}",
            HEADER_VARS_LEN,
            hv_bytes.len(),
        )));
    }
    out.extend_from_slice(&hv_bytes);

    // ------------------------------------------------------------
    // (4) Two-byte CRC placeholder (RS=0). LibreDWG patches the
    //     real CRC at offset `entities_start - 18` after every
    //     other section has been emitted; we mirror that here.
    // ------------------------------------------------------------
    let crc_offset = out.len();
    out.extend_from_slice(&[0u8, 0u8]);

    // ------------------------------------------------------------
    // (5) ENTITIES region (BEGIN sentinel + payload + END sentinel).
    // ------------------------------------------------------------
    out.extend_from_slice(&ENTITIES_BEGIN);
    let entities_start = u32_from_offset(out.len())?;
    out.extend_from_slice(&input.entities);
    let entities_end = u32_from_offset(out.len())?;
    out.extend_from_slice(&ENTITIES_END);

    debug_assert_eq!(crc_offset + 2 + SENTINEL_LEN, entities_start as usize);

    // ------------------------------------------------------------
    // (6) Ten empty per-table sections (BEGIN + END sentinels only).
    // ------------------------------------------------------------
    for (begin, end) in EMPTY_TABLE_SENTINELS.iter() {
        out.extend_from_slice(begin);
        out.extend_from_slice(end);
    }

    // ------------------------------------------------------------
    // (7) BLOCK_ENTITIES region.
    //
    //     Per `encode.c:3107-3108`, `blocks_size` carries an
    //     `0x40000000` flag whenever `version > R_2_22`, which is
    //     always true for R12 (= R_11 in LibreDWG's enum, ordinal
    //     0x10 > R_2_22 ordinal 0x07). Without the flag, LibreDWG
    //     reads `blocks_size` as zero and skips block parsing.
    // ------------------------------------------------------------
    out.extend_from_slice(&BLOCK_ENTITIES_BEGIN);
    let blocks_start = u32_from_offset(out.len())?;
    out.extend_from_slice(&input.block_entities);
    let blocks_payload_size = u32_from_offset(out.len())? - blocks_start;
    out.extend_from_slice(&BLOCK_ENTITIES_END);
    let blocks_size = encode_payload_size("block_entities", blocks_payload_size, BLOCKS_SIZE_FLAG)?;

    // ------------------------------------------------------------
    // (8) EXTRA_ENTITIES region. `extras_size` carries an
    //     `0x80000000` flag per `encode.c:3122-3124`.
    // ------------------------------------------------------------
    out.extend_from_slice(&EXTRA_ENTITIES_BEGIN);
    let extras_start = u32_from_offset(out.len())?;
    out.extend_from_slice(&input.extras);
    let extras_payload_size = u32_from_offset(out.len())? - extras_start;
    out.extend_from_slice(&EXTRA_ENTITIES_END);
    let extras_size = encode_payload_size("extras", extras_payload_size, EXTRAS_SIZE_FLAG)?;

    // ------------------------------------------------------------
    // (9) R11 aux header — sentinel-framed 138-byte block.
    // ------------------------------------------------------------
    let auxheader_address = u32_from_offset(out.len())?;
    encode_r11_auxheader(
        &mut out,
        &input.header_vars,
        AuxHeaderOffsets {
            entities_start,
            entities_end,
            blocks_start,
            extras_start,
            auxheader_address,
        },
    )?;

    // ------------------------------------------------------------
    // (10) Patch the section locator at 0x14..0x2C.
    // ------------------------------------------------------------
    patch_section_locator(
        &mut out,
        R12SectionLocator {
            entities_start,
            entities_end,
            blocks_start,
            blocks_size,
            extras_start,
            extras_size,
        },
    )?;

    // ------------------------------------------------------------
    // (11) Patch the 2-byte CRC at `entities_start - 18`, computed
    //      over bytes [0, entities_start - 18). The two reserved
    //      bytes from step (4) are NOT part of the input to the
    //      CRC because the CRC IS those two bytes.
    // ------------------------------------------------------------
    let crc = crc_x25(0xC0C1, &out[..crc_offset]);
    out[crc_offset..crc_offset + 2].copy_from_slice(&crc.to_le_bytes());

    Ok(out)
}

/// Decode an AC1009 file image into an [`R12Disassembly`].
pub fn disassemble(bytes: &[u8]) -> DwgResult<R12Disassembly> {
    // ------------------------------------------------------------
    // (1) File header + section locator at 0x14..0x2C.
    // ------------------------------------------------------------
    let file_header = R12FileHeader::parse(bytes)?;
    let locator = file_header.locator;

    // ------------------------------------------------------------
    // (2) Five leading section descriptors at 0x2C..0x5E.
    // ------------------------------------------------------------
    let leading = R12SectionTables::parse_leading_five(
        bytes
            .get(LEADING_SECTION_REGION_START..LEADING_SECTION_REGION_END)
            .ok_or(DwgError::UnexpectedEof {
                byte: LEADING_SECTION_REGION_END,
                bit: 0,
            })?,
    )?;

    // ------------------------------------------------------------
    // (3) Header variables (1631 bytes). Recovers the other five
    //     section descriptors (UCS/VPORT/APPID/DIMSTYLE/VX).
    // ------------------------------------------------------------
    let hv_start = LEADING_SECTION_REGION_END;
    if bytes.len() < hv_start + HEADER_VARS_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: hv_start + HEADER_VARS_LEN,
            bit: 0,
        });
    }
    let DecodedHeaderVars {
        header_vars,
        ucs_hdr,
        vport_hdr,
        appid_hdr,
        dimstyle_hdr,
        vx_hdr,
        consumed: _,
    } = R12HeaderVars::decode(&bytes[hv_start..hv_start + HEADER_VARS_LEN])?;

    let section_tables = R12SectionTables {
        block: leading[0],
        layer: leading[1],
        style: leading[2],
        ltype: leading[3],
        view: leading[4],
        ucs: ucs_hdr,
        vport: vport_hdr,
        appid: appid_hdr,
        dimstyle: dimstyle_hdr,
        vx: vx_hdr,
    };

    // ------------------------------------------------------------
    // (4) Validate the 2-byte CRC at `entities_start - 18`.
    // ------------------------------------------------------------
    let entities_start = locator.entities_start as usize;
    if entities_start < 18 {
        return Err(DwgError::MalformedObject {
            class: "R12 file header".to_string(),
            offset: 0x14,
            message: format!(
                "entities_start {entities_start:#x} is too small (need at least 18 bytes of preamble for the CRC)"
            ),
        });
    }
    let crc_at = entities_start - 18;
    if bytes.len() < crc_at + 2 {
        return Err(DwgError::UnexpectedEof {
            byte: crc_at + 2,
            bit: 0,
        });
    }
    let stored_crc = u16::from_le_bytes([bytes[crc_at], bytes[crc_at + 1]]);
    let computed_crc = crc_x25(0xC0C1, &bytes[..crc_at]);
    if stored_crc != computed_crc {
        return Err(DwgError::SectionCrcMismatch {
            section: "R12 pre-entities CRC",
            computed: u32::from(computed_crc),
            stored: u32::from(stored_crc),
        });
    }

    // ------------------------------------------------------------
    // (5) Verify ENTITIES_BEGIN sentinel at `entities_start - 16`.
    // ------------------------------------------------------------
    check_sentinel("ENTITIES_BEGIN", &ENTITIES_BEGIN, bytes, crc_at + 2)?;

    // ------------------------------------------------------------
    // (6) Read entities payload + ENTITIES_END sentinel.
    // ------------------------------------------------------------
    let entities_end = locator.entities_end as usize;
    if entities_end < entities_start || bytes.len() < entities_end + SENTINEL_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: entities_end + SENTINEL_LEN,
            bit: 0,
        });
    }
    let entities = bytes[entities_start..entities_end].to_vec();
    check_sentinel("ENTITIES_END", &ENTITIES_END, bytes, entities_end)?;

    // ------------------------------------------------------------
    // (7) Verify the ten empty per-table sentinel pairs.
    // ------------------------------------------------------------
    let mut cur = entities_end + SENTINEL_LEN;
    for (begin, end) in EMPTY_TABLE_SENTINELS.iter() {
        check_sentinel("R12 empty table BEGIN", begin, bytes, cur)?;
        cur += SENTINEL_LEN;
        check_sentinel("R12 empty table END", end, bytes, cur)?;
        cur += SENTINEL_LEN;
    }

    // ------------------------------------------------------------
    // (8) BLOCK_ENTITIES region.
    // ------------------------------------------------------------
    check_sentinel("BLOCK_ENTITIES_BEGIN", &BLOCK_ENTITIES_BEGIN, bytes, cur)?;
    cur += SENTINEL_LEN;
    let blocks_start = locator.blocks_start as usize;
    if cur != blocks_start {
        return Err(DwgError::MalformedObject {
            class: "R12 file header".to_string(),
            offset: 0x1C,
            message: format!(
                "blocks_start mismatch: locator says {blocks_start:#x}, file structure says {cur:#x}"
            ),
        });
    }
    let blocks_size_payload = (locator.blocks_size & SIZE_FIELD_MASK) as usize;
    if bytes.len() < cur + blocks_size_payload + SENTINEL_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: cur + blocks_size_payload + SENTINEL_LEN,
            bit: 0,
        });
    }
    let block_entities = bytes[cur..cur + blocks_size_payload].to_vec();
    cur += blocks_size_payload;
    check_sentinel("BLOCK_ENTITIES_END", &BLOCK_ENTITIES_END, bytes, cur)?;
    cur += SENTINEL_LEN;

    // ------------------------------------------------------------
    // (9) EXTRA_ENTITIES region.
    // ------------------------------------------------------------
    check_sentinel("EXTRA_ENTITIES_BEGIN", &EXTRA_ENTITIES_BEGIN, bytes, cur)?;
    cur += SENTINEL_LEN;
    let extras_start = locator.extras_start as usize;
    if cur != extras_start {
        return Err(DwgError::MalformedObject {
            class: "R12 file header".to_string(),
            offset: 0x24,
            message: format!(
                "extras_start mismatch: locator says {extras_start:#x}, file structure says {cur:#x}"
            ),
        });
    }
    let extras_size_payload = (locator.extras_size & SIZE_FIELD_MASK) as usize;
    if bytes.len() < cur + extras_size_payload + SENTINEL_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: cur + extras_size_payload + SENTINEL_LEN,
            bit: 0,
        });
    }
    let extras = bytes[cur..cur + extras_size_payload].to_vec();
    cur += extras_size_payload;
    check_sentinel("EXTRA_ENTITIES_END", &EXTRA_ENTITIES_END, bytes, cur)?;
    cur += SENTINEL_LEN;

    // ------------------------------------------------------------
    // (10) R11 aux header.
    // ------------------------------------------------------------
    check_sentinel("AUXHEADER_BEGIN", &AUXHEADER_BEGIN, bytes, cur)?;
    cur += SENTINEL_LEN;
    if bytes.len() < cur + AUX_HEADER_PAYLOAD_LEN + SENTINEL_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: cur + AUX_HEADER_PAYLOAD_LEN + SENTINEL_LEN,
            bit: 0,
        });
    }
    cur += AUX_HEADER_PAYLOAD_LEN;
    check_sentinel("AUXHEADER_END", &AUXHEADER_END, bytes, cur)?;

    Ok(R12Disassembly {
        header_vars,
        entities,
        block_entities,
        extras,
        section_tables,
        file_header,
    })
}

/// Offsets that the aux header needs to copy from the file header.
struct AuxHeaderOffsets {
    entities_start: u32,
    entities_end: u32,
    blocks_start: u32,
    extras_start: u32,
    auxheader_address: u32,
}

/// Emit the 170-byte R11 aux header (`encode.c:1728-1811`).
fn encode_r11_auxheader(
    out: &mut Vec<u8>,
    hv: &R12HeaderVars,
    off: AuxHeaderOffsets,
) -> DwgResult<()> {
    out.extend_from_slice(&AUXHEADER_BEGIN);
    let payload_start = out.len();

    // num_auxheader_variables [RS] — always 16 (encode.c:1740).
    out.extend_from_slice(&16u16.to_le_bytes());
    // auxheader_size [RS] — payload-only width (encode.c:1741).
    out.extend_from_slice(&(AUX_HEADER_PAYLOAD_LEN as u16).to_le_bytes());
    // entities_start, entities_end, blocks_start, extras_start [RLx].
    out.extend_from_slice(&off.entities_start.to_le_bytes());
    out.extend_from_slice(&off.entities_end.to_le_bytes());
    out.extend_from_slice(&off.blocks_start.to_le_bytes());
    out.extend_from_slice(&off.extras_start.to_le_bytes());
    // R11_HANDLING [RS] — copied from header_vars.HANDLING
    // (encode.c:1776-1777).
    out.extend_from_slice(&hv.handling.to_le_bytes());
    // HANDSEED [RL_BE] — copied from header_vars.HANDSEED
    // (encode.c:1779-1782). The encoder writes the *low* 32 bits in
    // big-endian byte order.
    let handseed = (hv.handseed & 0xFFFF_FFFF) as u32;
    out.extend_from_slice(&handseed.to_be_bytes());
    // plot_stamp [RL] — defaults to 0.
    out.extend_from_slice(&0u32.to_le_bytes());
    // num_aux_tables [RS] — always 10 (encode.c:1742).
    out.extend_from_slice(&10u16.to_le_bytes());

    // Ten `encode_preR13_section_chk` records (encode.c:1786-1804).
    // Each is RS(id) + RS(size) + RS(number) + RL(address) = 10 B.
    // For PR-F2 every descriptor is empty, but we still emit the
    // section ids in the spec-defined order.
    let chk = [
        (1u16, SectionTableHeader::default()),  // BLOCK
        (2u16, SectionTableHeader::default()),  // LAYER
        (3u16, SectionTableHeader::default()),  // STYLE
        (5u16, SectionTableHeader::default()),  // LTYPE (id 5 not 4!)
        (6u16, SectionTableHeader::default()),  // VIEW
        (7u16, SectionTableHeader::default()),  // UCS
        (8u16, SectionTableHeader::default()),  // VPORT
        (9u16, SectionTableHeader::default()),  // APPID
        (10u16, SectionTableHeader::default()), // DIMSTYLE
        (11u16, SectionTableHeader::default()), // VX
    ];
    for (id, hdr) in chk.iter() {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&hdr.size.to_le_bytes());
        out.extend_from_slice(&hdr.number.to_le_bytes());
        out.extend_from_slice(&hdr.address.to_le_bytes());
    }

    // auxheader_address [RLx].
    out.extend_from_slice(&off.auxheader_address.to_le_bytes());

    // 2-byte CRC over the aux header payload starting AT the
    // payload offset. LibreDWG's `bit_write_CRC (dat,
    // auxheader_address + 16, 0xC0C1)` computes the CRC over bytes
    // [auxheader_address + 16, dat->byte), i.e. the 136 bytes that
    // come after the BEGIN sentinel and before the CRC itself
    // (encode.c:1806).
    let crc = crc_x25(0xC0C1, &out[payload_start..]);
    out.extend_from_slice(&crc.to_le_bytes());

    // Verify the payload width matches the field we just wrote.
    let payload_end = out.len();
    let payload_width = payload_end - payload_start;
    if payload_width != AUX_HEADER_PAYLOAD_LEN {
        return Err(DwgError::InternalInvariant(format!(
            "aux header payload expected {AUX_HEADER_PAYLOAD_LEN} bytes, wrote {payload_width}"
        )));
    }

    out.extend_from_slice(&AUXHEADER_END);
    Ok(())
}

fn check_sentinel(name: &'static str, want: &Sentinel, bytes: &[u8], at: usize) -> DwgResult<()> {
    if bytes.len() < at + SENTINEL_LEN {
        return Err(DwgError::UnexpectedEof {
            byte: at + SENTINEL_LEN,
            bit: 0,
        });
    }
    if &bytes[at..at + SENTINEL_LEN] != want {
        let mut got = [0u8; SENTINEL_LEN];
        got.copy_from_slice(&bytes[at..at + SENTINEL_LEN]);
        return Err(DwgError::InvalidSentinel {
            section: name,
            expected: *want,
            got,
        });
    }
    Ok(())
}

fn u32_from_offset(byte_offset: usize) -> DwgResult<u32> {
    u32::try_from(byte_offset).map_err(|_| DwgError::WriteOverflow {
        limit: u32::MAX as usize,
    })
}

/// Encode a payload-length into the AC1009 `blocks_size` / `extras_size`
/// field: an explicit overflow check against [`SIZE_FIELD_MASK`]
/// (LibreDWG `decode_r11.c` truncates to 24 bits, so any payload past
/// that ceiling would be silently corrupted on read) plus the
/// type-flag OR. Keeps the encode and decode paths symmetric: the
/// disassembler always reads back `size & SIZE_FIELD_MASK`, so we
/// guarantee here that the payload never overlaps the flag bits.
fn encode_payload_size(region: &'static str, payload_size: u32, flag_bit: u32) -> DwgResult<u32> {
    if payload_size > SIZE_FIELD_MASK {
        return Err(DwgError::WriteOverflow {
            limit: SIZE_FIELD_MASK as usize,
        });
    }
    debug_assert_eq!(
        payload_size & flag_bit,
        0,
        "encode_payload_size({region}): payload bit overlaps {flag_bit:#010x} flag bit"
    );
    let _ = region;
    Ok(payload_size | flag_bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_assembly_round_trips() {
        let want = R12Assembly::empty();
        let bytes = assemble(&want).unwrap();
        let got = disassemble(&bytes).unwrap();
        assert_eq!(got.header_vars, want.header_vars);
        assert_eq!(got.entities, want.entities);
        assert_eq!(got.block_entities, want.block_entities);
        assert_eq!(got.extras, want.extras);
    }

    #[test]
    fn empty_assembly_has_expected_file_size() {
        let bytes = assemble(&R12Assembly::empty()).unwrap();
        // Breakdown for an empty document:
        //    44  file header
        // +  50  five leading section descriptors
        // +1631  header_vars (incl. the other five descriptors)
        // +   2  pre-entities CRC
        // +  16  ENTITIES_BEGIN sentinel
        // +   0  entities payload
        // +  16  ENTITIES_END sentinel
        // + 320  ten empty per-table sections (2 sentinels each)
        // +  32  BLOCK_ENTITIES_{BEGIN,END} (no payload)
        // +  32  EXTRA_ENTITIES_{BEGIN,END} (no payload)
        // + 170  AUXHEADER (BEGIN + 138 payload + END)
        // -----
        //  2313
        assert_eq!(bytes.len(), 2313);
    }

    #[test]
    fn assembly_locator_matches_layout() {
        let bytes = assemble(&R12Assembly::empty()).unwrap();
        let header = R12FileHeader::parse(&bytes).unwrap();
        // entities_start = 44 + 50 + 1631 + 2 + 16 = 1743 = 0x6CF.
        assert_eq!(header.locator.entities_start, 0x6CF);
        // entities_end == entities_start (no entities payload).
        assert_eq!(header.locator.entities_end, 0x6CF);
        // blocks_start = entities_end + 16 (ENTITIES_END) + 10 * 32
        //              (empty tables) + 16 (BLOCK_ENTITIES_BEGIN).
        assert_eq!(header.locator.blocks_start, 0x6CF + 16 + 320 + 16);
        // blocks_size has the 0x40000000 flag, payload is 0.
        assert_eq!(header.locator.blocks_size, 0x4000_0000);
        // extras_start = blocks_start + 16 (BLOCK_ENTITIES_END) + 16
        //              (EXTRA_ENTITIES_BEGIN).
        assert_eq!(header.locator.extras_start, 0x6CF + 16 + 320 + 16 + 16 + 16);
        // extras_size has the 0x80000000 flag, payload is 0.
        assert_eq!(header.locator.extras_size, 0x8000_0000);
    }

    #[test]
    fn assembly_pre_entities_crc_validates() {
        let bytes = assemble(&R12Assembly::empty()).unwrap();
        // disassemble already checks the CRC; we re-check here so
        // a regression that swallows the error fails this test
        // directly.
        let entities_start = R12FileHeader::parse(&bytes).unwrap().locator.entities_start as usize;
        let crc_at = entities_start - 18;
        let stored = u16::from_le_bytes([bytes[crc_at], bytes[crc_at + 1]]);
        let computed = crc_x25(0xC0C1, &bytes[..crc_at]);
        assert_eq!(stored, computed);
    }

    #[test]
    fn assembly_with_entities_payload_round_trips() {
        let mut assembly = R12Assembly::empty();
        // The wire format is opaque at this layer, so any byte
        // sequence will round-trip — `entity.rs`/`record_kinds.rs`
        // are responsible for producing a valid record stream.
        assembly.entities = vec![0xAA, 0xBB, 0xCC, 0xDD, 0x01, 0x02, 0x03, 0x04];
        let bytes = assemble(&assembly).unwrap();
        let got = disassemble(&bytes).unwrap();
        assert_eq!(got.entities, assembly.entities);
        // The locator must reflect the payload size.
        assert_eq!(
            got.file_header.locator.entities_end - got.file_header.locator.entities_start,
            assembly.entities.len() as u32
        );
    }

    #[test]
    fn assembly_corrupted_crc_is_rejected() {
        let mut bytes = assemble(&R12Assembly::empty()).unwrap();
        let entities_start = R12FileHeader::parse(&bytes).unwrap().locator.entities_start as usize;
        bytes[entities_start - 18] ^= 0x01;
        let err = disassemble(&bytes).unwrap_err();
        assert!(matches!(
            err,
            DwgError::SectionCrcMismatch {
                section: "R12 pre-entities CRC",
                ..
            }
        ));
    }

    #[test]
    fn assembly_corrupted_sentinel_is_rejected() {
        let mut bytes = assemble(&R12Assembly::empty()).unwrap();
        let entities_start = R12FileHeader::parse(&bytes).unwrap().locator.entities_start as usize;
        // Flip the first byte of the ENTITIES_BEGIN sentinel.
        bytes[entities_start - SENTINEL_LEN] ^= 0xFF;
        let err = disassemble(&bytes).unwrap_err();
        assert!(matches!(
            err,
            DwgError::InvalidSentinel {
                section: "ENTITIES_BEGIN",
                ..
            }
        ));
    }

    #[test]
    fn assembly_with_handseed_round_trips() {
        let hv = R12HeaderVars {
            handseed: 0x0000_0000_DEAD_BEEF,
            handling: 0x1234,
            ..R12HeaderVars::default()
        };
        let assembly = R12Assembly {
            header_vars: hv.clone(),
            ..R12Assembly::empty()
        };
        let bytes = assemble(&assembly).unwrap();
        let got = disassemble(&bytes).unwrap();
        assert_eq!(got.header_vars.handseed, hv.handseed);
        assert_eq!(got.header_vars.handling, hv.handling);
    }

    #[test]
    fn encode_payload_size_rejects_oversized_payload() {
        // 24-bit ceiling: anything > 0x00FF_FFFF must error out so the
        // wire format never silently truncates on the read path.
        let too_big = SIZE_FIELD_MASK + 1;
        let err = encode_payload_size("test", too_big, BLOCKS_SIZE_FLAG).unwrap_err();
        assert!(
            matches!(err, DwgError::WriteOverflow { limit } if limit == SIZE_FIELD_MASK as usize),
            "expected WriteOverflow {{ limit: {:#x} }}, got {err:?}",
            SIZE_FIELD_MASK
        );
    }

    #[test]
    fn encode_payload_size_preserves_max_24_bit_payload() {
        // At the ceiling itself we OR in the flag bit cleanly with no
        // bit overlap (regression test for the deleted `0x8FFF_FFFF`
        // typo mask: that mask cleared bits 28-30, which would have
        // truncated `SIZE_FIELD_MASK` from 0x00FFFFFF to 0x008FFFFF).
        let max_payload = SIZE_FIELD_MASK;
        let blocks = encode_payload_size("blocks", max_payload, BLOCKS_SIZE_FLAG).unwrap();
        assert_eq!(blocks, max_payload | BLOCKS_SIZE_FLAG);
        assert_eq!(blocks & SIZE_FIELD_MASK, max_payload);
        let extras = encode_payload_size("extras", max_payload, EXTRAS_SIZE_FLAG).unwrap();
        assert_eq!(extras, max_payload | EXTRAS_SIZE_FLAG);
        assert_eq!(extras & SIZE_FIELD_MASK, max_payload);
    }
}
