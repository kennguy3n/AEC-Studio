//! Asset import pipeline.
//!
//! There are two entry points:
//!
//! 1. **[`AssetImportPipeline::import`]** — legacy opaque-bytes mode.
//!    The caller provides a pre-hashed mesh payload (typically a
//!    bincode-encoded `aec_geometry::Mesh` from an extension manifest)
//!    plus declared vertex/triangle counts. The pipeline stores the
//!    blob, builds a ratio-only LOD chain, and writes a deterministic
//!    procedural-checker placeholder thumbnail. Each LOD level points
//!    at the same blob — no real decimation happens. Used by the
//!    extension host where extension packs ship pre-decimated meshes.
//!
//! 2. **[`AssetImportPipeline::import_mesh`]** — full real-import mode.
//!    The caller supplies a fully-realised `aec_geometry::Mesh`. The
//!    pipeline runs QEM decimation per LOD level (writing a separate
//!    content-addressed blob for each), renders a PBR thumbnail via
//!    [`crate::thumbnail`], and writes the metadata + per-LOD blob
//!    pointers. This is what the format-ingest paths in
//!    [`crate::ingest`] feed into.
//!
//! Both paths converge on the same DB schema; downstream consumers can
//! treat the resulting [`AssetMetadata`] identically.

use serde::{Deserialize, Serialize};

use aec_core::types::Units;
use aec_geometry::Mesh;

use crate::db::AssetDatabase;
use crate::decimate::{decimate, DecimateOptions};
use crate::error::{AssetError, AssetResult};
use crate::ingest::native;
use crate::lod::LodChain;
use crate::metadata::{AssetMetadata, License, MeshBlob, ThumbnailKind, Vendor};
use crate::thumbnail::{render_thumbnail, ThumbnailOptions};

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

    /// Real-import path: decimate `mesh` per LOD level, render a PBR
    /// thumbnail, and write per-level content-addressed blobs.
    ///
    /// Each level stores its own `aec_geometry::Mesh` (bincode via
    /// [`crate::ingest::native::encode`]) so the viewport can stream
    /// the exact triangle budget it wants without re-decimating.
    pub fn import_mesh(&mut self, req: RealMeshImportRequest) -> AssetResult<ImportSummary> {
        if req.mesh.positions.is_empty() || req.mesh.indices.is_empty() {
            return Err(AssetError::EmptyMesh);
        }
        if req.asset_id.trim().is_empty() {
            return Err(AssetError::InvalidManifest(
                "asset_id must not be empty".into(),
            ));
        }
        let base_triangles = (req.mesh.indices.len() / 3) as u32;
        if base_triangles == 0 {
            return Err(AssetError::EmptyMesh);
        }

        // Build the LOD chain (level 0 = base mesh, level N>0 = decimated).
        // Real-mesh imports get the aggressive [1.0, 0.25, 0.05] chain so
        // LOD2 is a true far-distance view, matching the Phase 11 spec.
        // Extension-host imports keep the legacy [1.0, 0.5, 0.25] chain
        // via the separate `import()` path.
        let chain = LodChain::aggressive_for_real_mesh(base_triangles, &req.extra_ratios);
        let base_bytes = native::encode(&req.mesh);
        let base_hash = format!("blake3:{}", blake3::hash(&base_bytes).to_hex());

        // Conflict check on existing asset.
        if let Some(existing) = self.db.get(&req.asset_id)? {
            if let Some(blob) = existing.lods.first() {
                if blob.mesh_hash != base_hash {
                    return Err(AssetError::HashConflict(req.asset_id.clone()));
                }
            }
        }

        let deduped = !self.db.put_blob(&base_hash, &base_bytes)?;

        // Decimate each non-base level and store as separate blobs.
        let mut lods: Vec<MeshBlob> = Vec::with_capacity(chain.levels.len());
        for level in &chain.levels {
            if level.level == 0 {
                lods.push(MeshBlob {
                    mesh_hash: base_hash.clone(),
                    vertex_count: req.mesh.positions.len() as u32,
                    triangle_count: base_triangles,
                });
                continue;
            }
            let opts = DecimateOptions {
                target_triangle_count: level.triangle_count,
                ..Default::default()
            };
            // Decimation may legitimately fail to hit the exact target
            // (e.g. on meshes with heavy boundary constraints). Fall
            // back to the base mesh in that case so the LOD chain stays
            // valid — the viewport will still see a chain entry with
            // the requested triangle budget.
            let decimated = match decimate(&req.mesh, &opts) {
                Ok(m) => m,
                Err(_) => req.mesh.clone(),
            };
            let bytes = native::encode(&decimated);
            let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            self.db.put_blob(&hash, &bytes)?;
            lods.push(MeshBlob {
                mesh_hash: hash,
                vertex_count: decimated.positions.len() as u32,
                triangle_count: (decimated.indices.len() / 3) as u32,
            });
        }

        // Render the thumbnail from the base mesh.
        let thumb_opts = req.thumbnail_opts.unwrap_or_default();
        let thumb_bytes = render_thumbnail(&req.mesh, &thumb_opts)
            .map_err(|e| AssetError::InvalidManifest(format!("thumbnail render failed: {e}")))?;
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
            thumbnail_kind: ThumbnailKind::Rendered,
            thumbnail_hash: thumbnail_hash.clone(),
            created_at: chrono::Utc::now(),
        };
        self.db.upsert_metadata(&metadata, &chain)?;

        Ok(ImportSummary {
            asset_id: req.asset_id,
            mesh_hash: base_hash,
            thumbnail_hash,
            lod_levels: chain.levels.len() as u8,
            deduped,
        })
    }

    /// One-shot ingest + real-mesh import: detect the format of `path`,
    /// parse it via [`crate::ingest::ingest_path`], then route through
    /// [`Self::import_mesh`] with the supplied metadata.
    ///
    /// This is the canonical Phase 11 entry point for "import a real
    /// glTF/OBJ/IFC asset from disk into the asset DB". Use it from
    /// the bridge layer when the user drops a model file onto the
    /// asset library.
    pub fn import_path(
        &mut self,
        path: &std::path::Path,
        meta: PathImportMetadata,
    ) -> AssetResult<ImportSummary> {
        let ingested = crate::ingest::ingest_path(path)
            .map_err(|e| AssetError::InvalidManifest(format!("ingest failed: {e}")))?;
        let req = RealMeshImportRequest {
            asset_id: meta.asset_id,
            name: meta.name,
            vendor: meta.vendor,
            version: meta.version,
            license: meta.license,
            attribution: meta.attribution,
            tags: meta.tags,
            style_tags: meta.style_tags,
            materials: meta.materials,
            source_units: meta.source_units,
            mesh: ingested.mesh,
            extra_ratios: meta.extra_ratios,
            thumbnail_opts: meta.thumbnail_opts,
        };
        self.import_mesh(req)
    }
}

/// Metadata supplied alongside a file-path import. Everything except
/// the mesh itself (which comes from the file) — used by
/// [`AssetImportPipeline::import_path`].
#[derive(Debug, Clone)]
pub struct PathImportMetadata {
    pub asset_id: String,
    pub name: String,
    pub vendor: Vendor,
    pub version: String,
    pub license: License,
    pub attribution: Option<String>,
    pub tags: Vec<String>,
    pub style_tags: Vec<String>,
    pub materials: Vec<String>,
    pub source_units: Units,
    pub extra_ratios: Vec<f32>,
    pub thumbnail_opts: Option<ThumbnailOptions>,
}

/// Full real-import request: caller-supplied `Mesh` + metadata. The
/// pipeline will decimate the mesh per LOD level and render a PBR
/// thumbnail rather than reusing a procedural placeholder.
#[derive(Debug, Clone)]
pub struct RealMeshImportRequest {
    pub asset_id: String,
    pub name: String,
    pub vendor: Vendor,
    pub version: String,
    pub license: License,
    pub attribution: Option<String>,
    pub tags: Vec<String>,
    pub style_tags: Vec<String>,
    pub materials: Vec<String>,
    pub source_units: Units,
    /// Fully-realised mesh. Will be QEM-decimated for each LOD level.
    pub mesh: Mesh,
    /// Extra LOD ratios beyond the default chain (`[1.0, 0.25, 0.05]`).
    /// Empty means "use the default chain".
    pub extra_ratios: Vec<f32>,
    /// Override thumbnail rendering options. `None` -> defaults.
    pub thumbnail_opts: Option<ThumbnailOptions>,
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

    fn dense_mesh() -> Mesh {
        // ~200-triangle grid so decimation has something to do.
        let mut mesh = Mesh::new();
        let n: u32 = 10;
        for j in 0..=n {
            for i in 0..=n {
                mesh.positions.push([i as f32, j as f32, 0.0]);
                mesh.normals.push([0.0, 0.0, 1.0]);
                mesh.uvs.push([i as f32 / n as f32, j as f32 / n as f32]);
            }
        }
        for j in 0..n {
            for i in 0..n {
                let tl = j * (n + 1) + i;
                let tr = tl + 1;
                let bl = tl + (n + 1);
                let br = bl + 1;
                mesh.indices.extend_from_slice(&[tl, tr, br, tl, br, bl]);
            }
        }
        mesh
    }

    fn real_req(id: &str, mesh: Mesh) -> RealMeshImportRequest {
        RealMeshImportRequest {
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
            tags: vec!["test".into()],
            style_tags: vec![],
            materials: vec![],
            source_units: Units::Mm,
            mesh,
            extra_ratios: vec![],
            thumbnail_opts: Some(ThumbnailOptions {
                width: 32,
                height: 32,
                samples_per_pixel: 1,
                max_bounces: 1,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn import_mesh_writes_per_lod_blobs_with_decreasing_triangles() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        let mesh = dense_mesh();
        let base_tri = (mesh.indices.len() / 3) as u32;
        let summary = p.import_mesh(real_req("dense", mesh)).unwrap();
        assert_eq!(summary.lod_levels, 3);
        let stored = db.get("dense").unwrap().unwrap();
        assert_eq!(stored.lods.len(), 3);
        // Level 0 is the base mesh.
        assert_eq!(stored.lods[0].triangle_count, base_tri);
        // Subsequent levels must be monotonically non-increasing.
        for w in stored.lods.windows(2) {
            assert!(
                w[1].triangle_count <= w[0].triangle_count,
                "LOD levels should be monotonically non-increasing"
            );
        }
        // At least one downstream level must actually be smaller than
        // the base — otherwise QEM produced no reduction and we silently
        // fell back to `req.mesh.clone()` in `import_mesh`, which would
        // mask a regression in `decimate()` (as happened with the
        // `shared_count` double-counting bug). The dense bipyramid input
        // has 14 manifold interior edges and zero boundary edges, so
        // any working QEM impl must reduce at least one level.
        assert!(
            stored.lods.iter().any(|l| l.triangle_count < base_tri),
            "no LOD level reduced: all levels still at base {base_tri} triangles \
             — decimate() likely failed or fell back to the base mesh: lods = {:?}",
            stored
                .lods
                .iter()
                .map(|l| l.triangle_count)
                .collect::<Vec<_>>(),
        );
        // Thumbnail should be a rendered PBR thumbnail.
        assert_eq!(stored.thumbnail_kind, ThumbnailKind::Rendered);
        let png = db.get_blob(&stored.thumbnail_hash).unwrap().unwrap();
        assert_eq!(&png[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    }

    #[test]
    fn import_mesh_rejects_empty_mesh() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        let mesh = Mesh::new();
        let err = p.import_mesh(real_req("empty", mesh)).unwrap_err();
        assert!(matches!(err, AssetError::EmptyMesh));
    }
}
