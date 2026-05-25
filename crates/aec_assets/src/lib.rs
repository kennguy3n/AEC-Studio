//! `aec_assets` — content-addressed asset pipeline.
//!
//! Assets enter through [`pipeline::AssetImportPipeline`] which validates the
//! manifest, normalizes unit/transforms, computes an LOD chain, hashes the
//! mesh blob with BLAKE3, then persists into [`db::AssetDatabase`]. Queries
//! filter by tag, style, vendor, or name and stream metadata back.

pub mod db;
pub mod decimate;
pub mod error;
pub mod extension_host;
pub mod ingest;
pub mod lod;
pub mod metadata;
pub mod pipeline;
pub mod query;
pub mod search;
pub mod thumbnail;
pub mod worker;

pub use db::AssetDatabase;
pub use decimate::{decimate as decimate_mesh, DecimateError, DecimateOptions, Quadric};
pub use error::{AssetError, AssetResult};
pub use extension_host::{install_asset_packs, AssetExtensionError, InstallSummary};
pub use ingest::{
    detect_format, ingest_bytes, ingest_path, IngestError, IngestFormat, IngestedMesh,
};
pub use lod::{LodChain, LodLevel};
pub use metadata::{AssetMetadata, License, MeshBlob, ThumbnailKind, Vendor};
pub use pipeline::{
    AssetImportPipeline, ImportRequest, ImportSummary, PathImportMetadata, RealMeshImportRequest,
};
pub use query::AssetQuery;
pub use search::{SearchHit, SearchOptions};
pub use thumbnail::{render_thumbnail, ThumbnailError, ThumbnailOptions};
pub use worker::{IngestJob, IngestPool, IngestPoolConfig, IngestResult, JobOutcome};
