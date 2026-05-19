use thiserror::Error;

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
}
