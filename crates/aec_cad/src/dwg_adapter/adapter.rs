//! Trait + error type for DWG↔DXF converters.
//!
//! DXF is the canonical format. DWG support is intentionally an
//! out-of-process adapter — the user supplies the converter binary
//! (e.g. ODA File Converter) and we shell out to it.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DwgError {
    #[error("DWG converter binary not configured")]
    NotConfigured,
    #[error("DWG converter binary not found at {0}")]
    BinaryMissing(PathBuf),
    #[error("DWG converter spawn failed: {0}")]
    SpawnFailed(String),
    #[error("DWG converter exited with status {code}: {stderr}")]
    ConverterFailed { code: i32, stderr: String },
    #[error("input file does not exist: {0}")]
    InputMissing(PathBuf),
    #[error("output file not produced: {0}")]
    OutputMissing(PathBuf),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

pub type DwgResult<T> = Result<T, DwgError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwgVersion {
    R12,
    R14,
    R2000,
    R2004,
    R2007,
    R2010,
    R2013,
    R2018,
}

/// Common interface for DWG↔DXF converters. Implementations are
/// stateless apart from binary path/config — they shell out to the
/// configured converter for every call.
pub trait DwgConverter: Send + Sync {
    /// Convert DWG → DXF. Returns the produced DXF path.
    fn dwg_to_dxf(&self, input_dwg: &Path, output_dxf: &Path) -> DwgResult<()>;

    /// Convert DXF → DWG. Returns the produced DWG path.
    fn dxf_to_dwg(&self, input_dxf: &Path, output_dwg: &Path, version: DwgVersion)
        -> DwgResult<()>;

    /// Human-readable name of the underlying converter (for logs and UI).
    fn name(&self) -> &'static str;

    /// Verify the configured binary is present and runnable.
    fn check(&self) -> DwgResult<()>;
}
