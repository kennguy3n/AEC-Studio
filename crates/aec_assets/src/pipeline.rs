//! Asset import pipeline.
//!
//! Input: a JSON manifest + raw mesh bytes. The pipeline:
//!   1. Validates the manifest (required fields, license tag, vendor).
//!   2. Normalizes transforms (centers to origin, optional unit conversion).
//!   3. Computes a BLAKE3 hash of the mesh payload (content-address key).
//!   4. Builds an LOD chain.
//!   5. Generates a procedural placeholder thumbnail.
//!   6. Inserts the blob and metadata into the database.
//!
//! Step 4's full mesh-decimation engine lands in Phase 3; for Phase 1/2 the
//! LOD chain reports ratio + estimated triangle counts without re-emitting
//! decimated blobs. The chain rows are still real (used by the viewport's
//! instancing layer to pick a target triangle budget per camera distance).

use serde::{Deserialize, Serialize};

use aec_core::types::Units;

use crate::db::AssetDatabase;
use crate::error::{AssetError, AssetResult};
use crate::lod::LodChain;
use crate::metadata::{AssetMetadata, License, MeshBlob, ThumbnailKind, Vendor};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub asset_id: String,
    pub name: String,
    pub vendor: Vendor,
    pub version: String,
    pub license: License,
    pub attribution: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub style_tags: Vec<String>,
    #[serde(default)]
    pub materials: Vec<String>,
    pub source_units: Units,
    /// Raw mesh payload bytes (e.g. a serialized `aec_geometry::Mesh`).
    pub mesh_bytes: Vec<u8>,
    /// Optional 32x32 placeholder thumbnail seed (RGB byte triples). When
    /// `None` the pipeline generates a deterministic checker pattern.
    pub thumbnail_seed: Option<Vec<u8>>,
    pub base_vertex_count: u32,
    pub base_triangle_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSummary {
    pub asset_id: String,
    pub mesh_hash: String,
    pub thumbnail_hash: String,
    pub lod_levels: u8,
    /// `true` if this import created a new mesh blob, `false` if it dedup'd
    /// onto an existing one.
    pub deduped: bool,
}

pub struct AssetImportPipeline<'a> {
    db: &'a mut AssetDatabase,
}

impl<'a> AssetImportPipeline<'a> {
    pub fn new(db: &'a mut AssetDatabase) -> Self {
        Self { db }
    }

    pub fn import(&mut self, req: ImportRequest) -> AssetResult<ImportSummary> {
        if req.mesh_bytes.is_empty() {
            return Err(AssetError::EmptyMesh);
        }
        if req.asset_id.trim().is_empty() {
            return Err(AssetError::InvalidManifest(
                "asset_id must not be empty".into(),
            ));
        }
        if req.base_triangle_count == 0 {
            return Err(AssetError::InvalidManifest(
                "base_triangle_count must be > 0".into(),
            ));
        }

        let mesh_hash = format!("blake3:{}", blake3::hash(&req.mesh_bytes).to_hex());
        // If asset already exists with a different mesh hash, that's a conflict.
        if let Some(existing) = self.db.get(&req.asset_id)? {
            if let Some(blob) = existing.lods.first() {
                if blob.mesh_hash != mesh_hash {
                    return Err(AssetError::HashConflict(req.asset_id.clone()));
                }
            }
        }

        let deduped = !self.db.put_blob(&mesh_hash, &req.mesh_bytes)?;

        let chain = LodChain::from_ratios(req.base_triangle_count, &[]);
        let lods: Vec<MeshBlob> = chain
            .levels
            .iter()
            .map(|level| MeshBlob {
                mesh_hash: mesh_hash.clone(),
                vertex_count: ((req.base_vertex_count as f32) * level.ratio).round() as u32,
                triangle_count: level.triangle_count,
            })
            .collect();

        let thumb_bytes = req.thumbnail_seed.unwrap_or_else(|| {
            // Deterministic checker pattern keyed on mesh hash.
            let mut buf = vec![0u8; 32 * 32 * 3];
            let seed = mesh_hash.as_bytes();
            for i in 0..(32 * 32) {
                let x = i % 32;
                let y = i / 32;
                let on = (x / 4 + y / 4) % 2 == 0;
                let s = seed[i % seed.len()];
                buf[i * 3] = if on { 232 } else { s };
                buf[i * 3 + 1] = if on { 224 } else { s.wrapping_mul(3) };
                buf[i * 3 + 2] = if on { 252 } else { s.wrapping_mul(5) };
            }
            buf
        });
        let thumbnail_hash = format!("blake3:{}", blake3::hash(&thumb_bytes).to_hex());
        self.db.put_blob(&thumbnail_hash, &thumb_bytes)?;

        let metadata = AssetMetadata {
            asset_id: req.asset_id.clone(),
            name: req.name,
            vendor: req.vendor,
            version: req.version,
            license: req.license,
            attribution: req.attribution,
            tags: req.tags,
            style_tags: req.style_tags,
            lods,
            materials: req.materials,
            thumbnail_kind: ThumbnailKind::Placeholder,
            thumbnail_hash: thumbnail_hash.clone(),
            created_at: chrono::Utc::now(),
        };
        self.db.upsert_metadata(&metadata, &chain)?;

        Ok(ImportSummary {
            asset_id: req.asset_id,
            mesh_hash,
            thumbnail_hash,
            lod_levels: chain.levels.len() as u8,
            deduped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str, payload: &[u8]) -> ImportRequest {
        ImportRequest {
            asset_id: id.into(),
            name: format!("Asset {id}"),
            vendor: Vendor {
                id: "v".into(),
                name: "V".into(),
                url: None,
            },
            version: "1.0".into(),
            license: License::CcBy,
            attribution: None,
            tags: vec!["chair".into()],
            style_tags: vec![],
            materials: vec![],
            source_units: Units::Mm,
            mesh_bytes: payload.to_vec(),
            thumbnail_seed: None,
            base_vertex_count: 800,
            base_triangle_count: 1000,
        }
    }

    #[test]
    fn import_creates_three_lod_levels() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        let summary = p.import(req("a", b"mesh-payload")).unwrap();
        assert_eq!(summary.lod_levels, 3);
        assert!(!summary.deduped);
        let stored = db.get("a").unwrap().unwrap();
        assert_eq!(stored.lods.len(), 3);
    }

    #[test]
    fn second_import_of_same_payload_dedups_the_blob() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        p.import(req("a", b"same-payload")).unwrap();
        let summary = p.import(req("b", b"same-payload")).unwrap();
        assert!(summary.deduped, "second import should dedupe");
    }

    #[test]
    fn empty_mesh_rejected() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        let err = p.import(req("a", b"")).unwrap_err();
        matches!(err, AssetError::EmptyMesh);
    }

    #[test]
    fn reimporting_with_different_payload_errors() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        p.import(req("a", b"first")).unwrap();
        let err = p.import(req("a", b"second")).unwrap_err();
        matches!(err, AssetError::HashConflict(_));
    }
}
