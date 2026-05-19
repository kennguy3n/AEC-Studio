//! `aec_assets` — content-addressed asset pipeline.
//!
//! Assets enter through [`pipeline::AssetImportPipeline`] which validates the
//! manifest, normalizes unit/transforms, computes an LOD chain, hashes the
//! mesh blob with BLAKE3, then persists into [`db::AssetDatabase`]. Queries
//! filter by tag, style, vendor, or name and stream metadata back.

pub mod db;
pub mod error;
pub mod lod;
pub mod metadata;
pub mod pipeline;
pub mod query;

pub use db::AssetDatabase;
pub use error::{AssetError, AssetResult};
pub use lod::{LodChain, LodLevel};
pub use metadata::{AssetMetadata, License, MeshBlob, ThumbnailKind, Vendor};
pub use pipeline::{AssetImportPipeline, ImportRequest, ImportSummary};
pub use query::AssetQuery;
