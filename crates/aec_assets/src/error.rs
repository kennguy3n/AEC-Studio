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
    ///
    /// ### Recovery
    ///
    /// The default LOD chain for real-mesh imports is the aggressive
    /// `[1.0, 0.25, 0.05]` per
    /// [`crate::LodChain::aggressive_for_real_mesh`]. On meshes with
    /// heavy boundary topology (open shells, non-manifold edges) the
    /// 5% target may be unreachable while the default decimation
    /// option `preserve_boundary: true` blocks every collapse that
    /// touches a boundary vertex. Two callers' knobs exist:
    ///
    /// * Set
    ///   [`crate::PathImportMetadata::decimate_options`] /
    ///   [`crate::pipeline::RealMeshImportRequest::decimate_options`]
    ///   to `Some(DecimateOptions { preserve_boundary: false, .. })`
    ///   to allow boundary collapses (silhouette will shrink at far
    ///   LODs, which is generally acceptable for icon-distance views).
    /// * Lower `DecimateOptions::max_cost` to cap how aggressively a
    ///   collapse is allowed to move geometry — useful in the
    ///   opposite direction, when the failure is from a high-cost
    ///   collapse hitting the cap rather than from boundary lock-in.
    ///   `max_cost` is evaluated in the canonical **mm² space**
    ///   (positions after the unit-canonicalisation step), not in
    ///   `source_units²`; see
    ///   [`crate::PathImportMetadata::decimate_options`] for the
    ///   conversion factor if you have a value calibrated for a
    ///   non-millimetre source unit.
    ///
    /// The `target_triangle_count` field on the supplied
    /// [`crate::DecimateOptions`] is **ignored**: the pipeline
    /// overrides it per-LOD-level from the chain. Only
    /// `preserve_boundary` and `max_cost` propagate.
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
    ///
    /// ### Recovery
    ///
    /// This typically fires on meshes with heavy boundary topology
    /// where every collapse the QEM solver tried was blocked by the
    /// default `preserve_boundary: true` option, leaving the
    /// decimated mesh tied with — or exceeding — the base triangle
    /// count. The recovery path is the same as for
    /// [`AssetError::Decimation`]: pass
    /// `Some(DecimateOptions { preserve_boundary: false, .. })` via
    /// [`crate::PathImportMetadata::decimate_options`] /
    /// [`crate::pipeline::RealMeshImportRequest::decimate_options`].
    /// See [`AssetError::Decimation`] for the full discussion.
    #[error(
        "decimation at LOD level {level} produced {actual} triangles, not strictly less than base {base}"
    )]
    LodNotStrictlyDecreasing { level: u8, actual: u32, base: u32 },
}
