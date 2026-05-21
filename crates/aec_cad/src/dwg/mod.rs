//! Native, in-process DWG (AutoCAD binary) reader and writer.
//!
//! Supports versions R12 → R2018 (AC1009 through AC1032). The canonical
//! in-memory representation is [`crate::dxf::DxfDocument`]; the DWG
//! codec converts between binary DWG bytes and that document type so
//! every existing entity (LINE, LWPOLYLINE, ARC, CIRCLE, ELLIPSE,
//! SPLINE, TEXT, MTEXT, INSERT, DIMENSION, HATCH, POLYLINE) plus the
//! LAYER / BLOCK_RECORD / DIMSTYLE / STYLE / LTYPE tables round-trips.
//!
//! ## Status
//!
//! The bit-stream codec, version dispatch, and file-structure
//! foundation are complete. Per-version entity round-trip is delivered
//! incrementally in this PR; see [`Reader::from_bytes`] for the
//! authoritative status per version.
//!
//! Anything the codec doesn't yet support produces a structured
//! [`DwgError::UnsupportedInVersion`] rather than silently corrupting
//! the file.

pub mod bits;
pub mod entities;
pub mod error;
pub mod file;
pub mod modern;
pub mod r12;
pub mod reader;
pub mod tables;
pub mod version;
pub mod writer;

pub use error::{DwgError, DwgResult};
pub use reader::DwgReader;
pub use version::{detect as detect_version, Version as DwgVersion};
pub use writer::DwgWriter;
