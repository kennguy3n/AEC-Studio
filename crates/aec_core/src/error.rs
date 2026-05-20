//! The shared error type for AEC Studio's core.

use thiserror::Error;

pub type AecResult<T> = Result<T, AecError>;

#[derive(Debug, Error)]
pub enum AecError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("invalid id for {kind}: {value:?}")]
    InvalidId { kind: &'static str, value: String },

    #[error("invalid project package at {path:?}: {reason}")]
    InvalidPackage { path: String, reason: String },

    #[error("invalid project manifest: {0}")]
    InvalidManifest(String),

    #[error("project schema version mismatch: file is v{found}, expected v{expected}")]
    SchemaMismatch { found: u32, expected: u32 },

    #[error("template '{0}' not found")]
    TemplateNotFound(String),

    #[error("invalid template definition: {0}")]
    InvalidTemplate(String),

    #[error("project package already exists at {0:?}")]
    AlreadyExists(String),

    #[error("invalid encryption key: {0}")]
    InvalidKey(String),

    #[error("recents store is corrupt: {0}")]
    CorruptRecents(String),

    #[error("OS random source unavailable: {0}")]
    Random(#[from] getrandom::Error),

    /// Catch-all for module-specific validation errors (e.g. empty
    /// revision tag, malformed comparison input). Prefer adding a
    /// typed variant when a downstream caller needs to branch on the
    /// specific error; use this for one-off string contexts.
    #[error("{0}")]
    Other(String),
}
