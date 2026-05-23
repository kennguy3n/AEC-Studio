//! AC1009 (AutoCAD R11/R12) format — distinct from R13+.
//!
//! R12 is structurally a different format: no bit-encoded objects, no
//! class section, fixed-record entity blocks. Trying to share a code
//! path with R13+ would be a constant source of subtle bugs, so this
//! subtree carries its own header parser, entity records, and table
//! records.
//!
//! The R12 byte-level layout (cross-checked against the published
//! AutoCAD R12 DXF/DWG reference manual):
//!
//! ```text
//! offset │ size │ meaning
//! ───────┼──────┼─────────────────────────────────────────
//!   0x00 │   6  │ "AC1009"
//!   0x06 │   6  │ Zero padding
//!   0x0c │   4  │ Image preview seeker (file-relative, 0 if none)
//!   0x10 │   4  │ Block table offset
//!   0x14 │   4  │ Layer table offset
//!   0x18 │   4  │ Linetype table offset
//!   0x1c │   4  │ Style table offset
//!   0x20 │   4  │ View table offset
//!   0x24 │   4  │ UCS table offset
//!   0x28 │   4  │ Viewport table offset
//!   0x2c │   4  │ DimStyle table offset
//!   0x30 │   4  │ AppId table offset
//!   0x34 │   4  │ Entities offset
//!   0x38 │   4  │ End-of-file offset
//!   0x3c │  …   │ Header variables (HEADER section in the older shape)
//! ```
//!
//! Each table is a sequence of fixed-size records (size depends on the
//! table). The entities section is a flat sequence of records terminated
//! by a sentinel byte. CRC is computed per-table using CRC-X25.

pub mod bridge;
pub mod entity;
pub mod file;
pub mod header;
pub mod header_vars;
pub mod reader;
pub mod record_kinds;
pub mod spec;
pub mod tables;
pub mod writer;

pub use header::R12FileHeader;
pub use reader::R12Reader;
pub use writer::R12Writer;
