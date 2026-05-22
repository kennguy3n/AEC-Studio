//! Top-level file structure for modern (R13+) DWG files.
//!
//! The on-disk layout (cross-checked against the OpenDesign spec and
//! verified against LibreDWG test fixtures) is:
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ 0x00 │ File signature "AC10NN" (6 bytes)     │
//! │ 0x06 │ 6 zero bytes (reserved)               │
//! │ 0x0c │ Maintenance release byte              │
//! │ 0x0d │ Image preview seeker (4-byte LE u32)  │
//! │ 0x11 │ Application/codepage flag (1 byte)    │
//! │ 0x12 │ Codepage (2-byte LE u16)              │
//! │ 0x14 │ R2004+: security flag / size hints    │
//! │ 0x16 │ Section locator records (R13–R2000)   │
//! │   ─  │  ─── or R2004+ system section ────    │
//! ├──────────────────────────────────────────────┤
//! │ Section: HEADER variables                    │
//! │ Section: CLASSES (R13+)                      │
//! │ Section: OBJECTS (entities, tables, …)       │
//! │ Section: OBJECT_MAP (handle → offset)        │
//! │ Section: SECOND_HEADER (checksum/duplicate)  │
//! │ Section: IMAGE (preview, optional)           │
//! │ Section: PADDING (to 64-byte boundary)       │
//! └──────────────────────────────────────────────┘
//! ```

pub mod classes;
pub mod header;
pub mod header_vars;
pub mod object_map;
pub mod pages;
pub mod r2000_layout;
pub mod r2004_layout;
pub mod r2007_header;
pub mod sections;
pub mod sentinels;
pub mod system_section;

pub use header::FileHeader;
pub use r2000_layout::{assemble_r2000, parse_r2000, R2000File, R2000FileParts, R2000Object};
pub use r2004_layout::{assemble_r2004, parse_r2004, R2004File, R2004FileParts};
pub use sections::{SectionId, SectionLocator};
