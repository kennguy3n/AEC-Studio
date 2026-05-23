//! AC1009 (AutoCAD R11/R12) format — distinct from R13+.
//!
//! R12 is structurally a different format from R13+: no
//! bit-encoded objects, no class section, fixed-record entity
//! blocks framed by 16-byte sentinels. Trying to share a code path
//! with R13+ would be a constant source of subtle bugs, so this
//! subtree carries its own header parser, entity records, and
//! wire-format assembler.
//!
//! The wire-format details live under [`spec`]. The high-level
//! file map is:
//!
//! ```text
//! offset │ region
//! ───────┼──────────────────────────────────────────────────
//!  0x00  │ 44-byte file header (AC1009 magic + section locator
//!        │ at 0x14..0x2C)
//!  0x2C  │ Five contiguous section descriptors (BLOCK, LAYER,
//!        │ STYLE, LTYPE, VIEW; 10 B each)
//!  0x5E  │ Header variables (1631 B) with the other five
//!        │ section descriptors (UCS, VPORT, APPID, DIMSTYLE,
//!        │ VX) interleaved at fixed offsets
//!  ┊     │ 2-byte CRC (computed over [0, this offset))
//!  ┊     │ DWG_SENTINEL_R11_ENTITIES_BEGIN + entities payload +
//!        │ DWG_SENTINEL_R11_ENTITIES_END
//!  ┊     │ Ten empty per-table sections (BEGIN + END sentinels
//!        │ each; no records emitted in PR-F2 scope)
//!  ┊     │ DWG_SENTINEL_R11_BLOCK_ENTITIES_BEGIN + payload +
//!        │ DWG_SENTINEL_R11_BLOCK_ENTITIES_END
//!  ┊     │ DWG_SENTINEL_R11_EXTRA_ENTITIES_BEGIN + payload +
//!        │ DWG_SENTINEL_R11_EXTRA_ENTITIES_END
//!  eof   │ R11 auxheader (sentinel-framed, 170 B total)
//! ```
//!
//! See [`spec::assemble`] for the assembler/disassembler and
//! [`spec::sentinels`] for the 28 hardcoded 16-byte magic
//! constants LibreDWG's `decode_preR13_sentinel` checks against.

pub mod bridge;
pub mod entity;
pub mod file;
pub mod reader;
pub mod record_kinds;
pub mod spec;
pub mod tables;
pub mod writer;

pub use reader::R12Reader;
pub use spec::file_header::R12FileHeader;
pub use writer::R12Writer;
