//! AC1009 wire-format spec modules.
//!
//! These modules encode/decode the actual AutoCAD R12 (AC1009) file
//! format as understood by LibreDWG (`PRE(R_13b1)` codepath). They
//! are kept under a `spec/` submodule so the older custom-format
//! parsers in the parent `r12` module can coexist while the
//! cut-over to the conformant assembler in [`super::file`] lands
//! phase-by-phase.
//!
//! Source-of-truth references in LibreDWG:
//!
//! * `src/common.c` lines 120-209 — the 28 sentinel byte patterns
//! * `src/header_variables_r11.spec` — the 198-field header block
//! * `src/encode.c` lines 2956-3187 — PRE(R_13b1) emit order
//! * `src/decode_r11.c` — the matching decoder

pub mod assemble;
pub mod file_header;
pub mod header_vars;
pub mod section_table;
pub mod sentinels;
