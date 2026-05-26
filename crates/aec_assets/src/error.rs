use thiserror::Error;

use crate::decimate::DecimateError;

pub type AssetResult<T> = std::result::Result<T, AssetError>;

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid asset manifest: {0}")]
    InvalidManifest(String),
    #[error("asset `{0}` not found")]
    NotFound(String),
    #[error("asset `{0}` already imported with a different hash")]
    HashConflict(String),
    #[error("empty mesh")]
    EmptyMesh,
    /// Format ingest failed; preserves the underlying `IngestError`
    /// variant so callers can distinguish unsupported-format /
    /// io / parse failures programmatically (not just by message text).
    #[error("ingest failed: {0}")]
    Ingest(#[from] crate::ingest::IngestError),
    /// QEM decimation could not produce a mesh that strictly reduces
    /// the triangle count at the requested LOD level. Carries the
    /// `level` index (1-based; LOD 0 is the base mesh and is never
    /// decimated), the requested triangle budget, and the source
    /// `DecimateError` for diagnostics. This is the structured
    /// replacement for the previous silent fallback that stored the
    /// base mesh under a degraded LOD hash.
    #[error("decimation failed at LOD level {level} (target = {target} triangles): {source}")]
    Decimation {
        level: u8,
        target: u32,
        #[source]
        source: DecimateError,
    },
    /// QEM decimation succeeded but produced a mesh whose triangle
    /// count is not strictly less than the base mesh's. Storing it
    /// under a separate LOD hash would either duplicate the base blob
    /// or claim a triangle budget the chain cannot honour, so the
    /// pipeline rejects the import early.
    #[error(
        "decimation at LOD level {level} produced {actual} triangles, not strictly less than base {base}"
    )]
    LodNotStrictlyDecreasing { level: u8, actual: u32, base: u32 },
}
