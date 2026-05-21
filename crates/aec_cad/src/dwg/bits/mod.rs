//! Bit-stream codec — the foundation of all R13+ DWG decoding/encoding.
//!
//! DWG R13+ stores everything past the file header as a bit-aligned
//! stream with variable-length scalar encodings. The encodings are
//! documented in the Open Design Alliance's *OpenDesign Specification
//! for .dwg files* § "Bit codes and data definitions". The shapes:
//!
//! | Type   | Description                                              | Bits        |
//! |--------|----------------------------------------------------------|-------------|
//! | **B**  | Single bit (0 or 1)                                      | 1           |
//! | **BB** | 2 bits — control selector                                | 2           |
//! | **BS** | Bit short: control bits 00→16-bit / 01→8-bit / 10→0 / 11→256 | 2+(0/8/16) |
//! | **BL** | Bit long:  control bits 00→32-bit / 01→8-bit / 10→0 / 11→reserved | 2+(0/8/32) |
//! | **BD** | Bit double: control bits 00→64-bit IEEE / 01→1.0 / 10→0.0 / 11→reserved | 2+(0/64) |
//! | **MC** | Modular char (signed) — 7-bit chunks with continuation   | 8n          |
//! | **MS** | Modular short (unsigned) — 15-bit LE chunks with continuation | 16n    |
//! | **H**  | Handle reference — 4-bit code, 4-bit byte count, bytes   | 8+8n        |
//! | **TV** | Text value: BS length, then ASCII/CP1252 bytes (R2004-)  | variable    |
//! | **T**  | Text (UTF-16LE): BS length, then UTF-16LE units (R2007+) | variable    |
//! | **CMC**| Color (RGB + ACI + name) — R2004+ extended ACI           | variable    |
//! | **3BD**| 3 × BD (point in 3-space)                                | 3×BD        |
//! | **2RD**| 2 × IEEE-754 64-bit raw (2D point)                       | 128         |
//! | **3RD**| 3 × IEEE-754 64-bit raw (3D point)                       | 192         |
//!
//! Round-trip property is enforced: for each primitive there's a
//! unit test that runs `write` then `read` and recovers both the
//! value and the bit-cursor offset.

pub mod crc;
pub mod reader;
pub mod writer;

pub use reader::BitReader;
pub use writer::BitWriter;
