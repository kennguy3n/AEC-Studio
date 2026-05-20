//! Out-of-process DWG↔DXF converter adapters.
//!
//! DXF is the canonical exchange format inside aec_cad. DWG is intentionally
//! a thin compatibility layer: we shell out to a user-configured binary
//! (ODA File Converter or LibreDWG `dwgread`/`dwgwrite`) to do the
//! conversion. The user must opt in by providing the binary path in
//! settings — no DWG converter ships with aec_cad itself.

pub mod adapter;
pub mod libredwg_adapter;
pub mod oda_adapter;

pub use adapter::{DwgConverter, DwgError, DwgResult, DwgVersion};
pub use libredwg_adapter::LibreDwgAdapter;
pub use oda_adapter::OdaAdapter;
