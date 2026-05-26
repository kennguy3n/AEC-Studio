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

        // Canonicalise the mesh to the asset DB's internal unit (mm).
        // The Mesh blob bytes (and therefore its BLAKE3 hash) reflect
        // the converted positions, so an identical mesh imported under
        // `Mm`, `M`, `Inches`, or `Feet` dedupes to the same blob.
        // The no-op `Mm` path borrows the caller's mesh; non-identity
        // conversions clone-then-scale so the original is untouched.
        let mesh_in_mm: std::borrow::Cow<'_, Mesh> =
            canonicalise_to_mm(&req.mesh, req.source_units);

        // Build the LOD chain (level 0 = base mesh, level N>0 = decimated).
        // Real-mesh imports get the aggressive [1.0, 0.25, 0.05] chain so
        // LOD2 is a true far-distance view, matching the Phase 11 spec.
        // Extension-host imports keep the legacy [1.0, 0.5, 0.25] chain
        // via the separate `import()` path.
        let chain = LodChain::aggressive_for_real_mesh(base_triangles, &req.extra_ratios);
        let base_bytes = native::encode(mesh_in_mm.as_ref());
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
        //
        // Failure modes are surfaced as structured `AssetError` variants
        // rather than silently substituting the base mesh under a
        // degraded LOD hash (which would either duplicate the base blob
        // or claim a triangle budget the chain cannot honour):
        //
        //   - `decimate()` itself errored  → `AssetError::Decimation`
        //   - decimated triangle count is not strictly less than the
        //     base count → `AssetError::LodNotStrictlyDecreasing`
        //
        // Decimation may legitimately fall short of the exact target
        // budget (e.g. on meshes with heavy boundary constraints), so
        // we only require strict reduction relative to the base mesh —
        // not relative to the requested `level.triangle_count`.
        let mut lods: Vec<MeshBlob> = Vec::with_capacity(chain.levels.len());
        for level in &chain.levels {
            if level.level == 0 {
                lods.push(MeshBlob {
                    mesh_hash: base_hash.clone(),
                    vertex_count: mesh_in_mm.positions.len() as u32,
                    triangle_count: base_triangles,
                });
                continue;
            }
            let opts = DecimateOptions {
                target_triangle_count: level.triangle_count,
                ..Default::default()
            };
            let decimated =
                decimate(mesh_in_mm.as_ref(), &opts).map_err(|source| AssetError::Decimation {
                    level: level.level,
                    target: level.triangle_count,
                    source,
                })?;
            let actual_triangles = (decimated.indices.len() / 3) as u32;
            if actual_triangles >= base_triangles {
                return Err(AssetError::LodNotStrictlyDecreasing {
                    level: level.level,
                    actual: actual_triangles,
                    base: base_triangles,
                });
            }
            let bytes = native::encode(&decimated);
            let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
            self.db.put_blob(&hash, &bytes)?;
            lods.push(MeshBlob {
                mesh_hash: hash,
                vertex_count: decimated.positions.len() as u32,
                triangle_count: actual_triangles,
            });
        }

        // Render the thumbnail from the canonicalised base mesh so the
        // preview reflects the same geometry that ships in the LOD 0
        // blob.
        let thumb_opts = req.thumbnail_opts.unwrap_or_default();
        let thumb_bytes = render_thumbnail(mesh_in_mm.as_ref(), &thumb_opts)
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
    ///
    /// # Source-units contract
    ///
    /// `meta.source_units` is taken verbatim and applied to the
    /// ingested mesh by [`canonicalise_to_mm`] before hashing. The
    /// pipeline does **not** auto-derive the unit from the detected
    /// format — doing so silently would mask caller bugs (e.g. a
    /// project convention that ships glTF in mm rather than the
    /// spec-default metres). Instead, the caller is expected to either
    /// (a) know the unit out-of-band, or (b) consult
    /// [`crate::ingest::IngestFormat::default_units`] to obtain the
    /// spec-defined default, optionally overriding it before this
    /// call. The bridge layer should default to
    /// `IngestFormat::default_units(detected_format)` and only deviate
    /// when the source file or project metadata explicitly says
    /// otherwise; the helper exists precisely so that mis-typed unit
    /// constants are caught by code review rather than silently
    /// applied as a 1000× scale error.
    pub fn import_path(
        &mut self,
        path: &std::path::Path,
        meta: PathImportMetadata,
    ) -> AssetResult<ImportSummary> {
        // Preserve `IngestError` variant granularity via
        // `AssetError::Ingest(#[from] IngestError)` so callers can
        // programmatically distinguish unsupported-format / io /
        // parse failures (not just by message text).
        let ingested = crate::ingest::ingest_path(path)?;
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
///
/// `source_units` is caller-supplied and is **not** derived from the
/// detected format; see [`AssetImportPipeline::import_path`] for the
/// rationale. Callers without out-of-band knowledge of the file's
/// authoring unit should default this field to
/// [`crate::ingest::IngestFormat::default_units`] for the format they
/// expect to detect.
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

/// Convert a mesh's vertex positions from `source_units` to the asset
/// DB's canonical internal unit (millimetres).
///
/// The `Cow` return type lets the hot path (“scale is the identity”)
/// borrow the caller's mesh and skip the allocation/copy entirely.
/// All other unit variants clone-then-scale so the caller's mesh is
/// untouched. Normals are scale-invariant under uniform scale and so
/// are not re-normalised; UVs are unit-agnostic.
///
/// The identity check is an `f32::EPSILON`-tolerant comparison rather
/// than `scale == 1.0`. With the current [`Units`] enum the only
/// identity is `Units::Mm` (other variants produce factors of 1000,
/// 25.4, or 304.8 — nowhere near 1.0), so for today exact equality
/// and the epsilon check are equivalent. The epsilon is
/// forward-compatibility: if a future variant is added with a factor
/// very close to (but not exactly) 1.0, exact equality would fall
/// through to clone-then-scale by a near-identity factor — a
/// performance pessimisation rather than a correctness bug, but one
/// that is easy to prevent here. The epsilon is also defensive
/// against any rounding noise introduced by `f64 -> f32` on the
/// scale factor itself.
fn canonicalise_to_mm(mesh: &Mesh, source_units: Units) -> std::borrow::Cow<'_, Mesh> {
    let scale = source_units.to_mm(1.0) as f32;
    if (scale - 1.0).abs() <= f32::EPSILON {
        return std::borrow::Cow::Borrowed(mesh);
    }
    let mut scaled = mesh.clone();
    for p in &mut scaled.positions {
        p[0] *= scale;
        p[1] *= scale;
        p[2] *= scale;
    }
    std::borrow::Cow::Owned(scaled)
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

    #[test]
    fn canonicalise_to_mm_borrows_when_units_are_already_mm() {
        // Hot path: `source_units == Mm` must not clone the mesh.
        let mesh = dense_mesh();
        let positions_before = mesh.positions.clone();
        let cow = canonicalise_to_mm(&mesh, Units::Mm);
        assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
        assert_eq!(cow.positions, positions_before);
    }

    #[test]
    fn canonicalise_to_mm_scales_metres_by_one_thousand() {
        // A single triangle at (1, 0, 0), (0, 1, 0), (0, 0, 0) in metres
        // must convert to (1000, 0, 0), (0, 1000, 0), (0, 0, 0) in mm.
        let mut mesh = Mesh::new();
        mesh.positions = vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]];
        mesh.normals = vec![[0.0, 0.0, 1.0]; 3];
        mesh.uvs = vec![[0.0, 0.0]; 3];
        mesh.indices = vec![0, 1, 2];

        let cow = canonicalise_to_mm(&mesh, Units::M);
        assert!(matches!(cow, std::borrow::Cow::Owned(_)));
        assert_eq!(
            cow.positions,
            vec![[1000.0, 0.0, 0.0], [0.0, 1000.0, 0.0], [0.0, 0.0, 0.0]]
        );
        // Original caller mesh untouched.
        assert_eq!(
            mesh.positions,
            vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]]
        );
    }

    #[test]
    fn canonicalise_to_mm_scales_inches_by_twenty_five_point_four() {
        let mut mesh = Mesh::new();
        mesh.positions = vec![[1.0, 0.0, 0.0]];
        mesh.normals = vec![[0.0, 0.0, 1.0]];
        mesh.uvs = vec![[0.0, 0.0]];
        mesh.indices = vec![0];

        let cow = canonicalise_to_mm(&mesh, Units::Inches);
        assert!((cow.positions[0][0] - 25.4).abs() < 1e-4);
    }

    fn unit_square_grid_mm() -> Mesh {
        // 4x4 grid of unit squares (32 triangles, 25 vertices). All
        // positions are integer multiples of 1 mm and exactly
        // representable in f32, AND scale losslessly to/from metres
        // (× 1/1000) since the metres values are also exact f32 values
        // (1.0/1000.0 == 0.001 exactly... no, actually 0.001 is *not*
        // exactly representable in f32). So instead pin positions at
        // *thousands* of mm, which factor cleanly: 1000.0 mm == 1.0 m,
        // both exactly representable.
        let mut mesh = Mesh::new();
        let n: u32 = 4;
        for j in 0..=n {
            for i in 0..=n {
                mesh.positions
                    .push([(i as f32) * 1000.0, (j as f32) * 1000.0, 0.0]);
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

    fn unit_square_grid_m() -> Mesh {
        // Same grid expressed in metres: every position divided by 1000
        // exactly (0..=4 metres → 0..=4000 mm).
        let mut mesh = unit_square_grid_mm();
        for p in &mut mesh.positions {
            p[0] /= 1000.0;
            p[1] /= 1000.0;
            p[2] /= 1000.0;
        }
        mesh
    }

    #[test]
    fn import_mesh_canonicalises_metres_to_mm_before_hashing() {
        // Two meshes representing the same geometry: one tagged metres
        // with values 0..=4, one tagged mm with values 0..=4000. After
        // canonicalisation they must hash to the same BLAKE3 blob —
        // proving that the pipeline actually applies `source_units`
        // rather than just forwarding the field.
        //
        // Positions are pinned at integer multiples of 1000 mm / 1 m
        // so both encodings are exactly representable in f32 (no
        // rounding drift between the two import paths).
        let metres_mesh = unit_square_grid_m();
        let mm_mesh = unit_square_grid_mm();

        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);

        let mut metres_req = real_req("metres", metres_mesh);
        metres_req.source_units = Units::M;
        let metres_summary = p.import_mesh(metres_req).unwrap();

        let mut mm_req = real_req("mm", mm_mesh);
        mm_req.source_units = Units::Mm;
        let mm_summary = p.import_mesh(mm_req).unwrap();

        assert_eq!(
            metres_summary.mesh_hash, mm_summary.mesh_hash,
            "metres-tagged and mm-tagged imports of the same geometry \
             must produce identical canonical hashes"
        );
        assert!(
            mm_summary.deduped,
            "second import (mm-tagged) should dedupe on the blob the \
             metres-tagged import already wrote"
        );
    }

    #[test]
    fn import_mesh_surfaces_decimation_error_instead_of_silent_fallback() {
        // A 1-triangle mesh cannot be decimated to anything smaller —
        // the QEM impl returns `DecimateError::TargetTooLarge` (target
        // > input). The old silent `req.mesh.clone()` fallback masked
        // this; the new structured path surfaces it.
        let mut mesh = Mesh::new();
        mesh.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        mesh.normals = vec![[0.0, 0.0, 1.0]; 3];
        mesh.uvs = vec![[0.0, 0.0]; 3];
        mesh.indices = vec![0, 1, 2];

        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut p = AssetImportPipeline::new(&mut db);
        let err = p.import_mesh(real_req("tiny", mesh)).unwrap_err();
        match err {
            AssetError::Decimation { level, .. } => {
                assert!(level >= 1, "LOD 0 is never decimated");
            }
            AssetError::LodNotStrictlyDecreasing { level, .. } => {
                assert!(
                    level >= 1,
                    "LOD 0 is never decimated and cannot trigger the strict-decrease guard"
                );
            }
            other => panic!("expected Decimation or LodNotStrictlyDecreasing, got: {other:?}"),
        }
    }

    #[test]
    fn canonicalise_to_mm_borrow_path_is_epsilon_tolerant() {
        // Forward-compat guard: the identity-scale check uses
        // `(scale - 1.0).abs() <= f32::EPSILON` rather than strict
        // `==`. With today's `Units` enum the only identity factor is
        // exact 1.0 (Units::Mm), so this test pins the *predicate* of
        // the comparison: a value differing from 1.0 by less than
        // `f32::EPSILON` must still be treated as the identity, while
        // a value differing visibly (e.g. the 25.4 from
        // `Units::Inches`) must not. If a future `Units` variant
        // produces a factor 1.0 ± tiny rounding noise, the hot-path
        // borrow must still trigger.
        let near_one_below = 1.0_f32 - (f32::EPSILON * 0.5);
        let near_one_above = 1.0_f32 + (f32::EPSILON * 0.5);
        assert!((near_one_below - 1.0).abs() <= f32::EPSILON);
        assert!((near_one_above - 1.0).abs() <= f32::EPSILON);

        let inches_scale = Units::Inches.to_mm(1.0) as f32;
        assert!((inches_scale - 1.0).abs() > f32::EPSILON);

        // And the actual function: `Units::Mm` (identity) still
        // borrows after the epsilon rewrite.
        let mesh = dense_mesh();
        let cow = canonicalise_to_mm(&mesh, Units::Mm);
        assert!(
            matches!(cow, std::borrow::Cow::Borrowed(_)),
            "Units::Mm must continue to hit the borrow fast path"
        );
    }

    #[test]
    fn ingest_format_default_units_matches_spec() {
        // Pins the spec-defined default unit per format. Pipelines
        // that consult `IngestFormat::default_units` to seed
        // `PathImportMetadata.source_units` rely on this exact table.
        use crate::ingest::IngestFormat;
        assert_eq!(IngestFormat::Gltf.default_units(), Units::M);
        assert_eq!(IngestFormat::Glb.default_units(), Units::M);
        assert_eq!(IngestFormat::Obj.default_units(), Units::Mm);
        assert_eq!(IngestFormat::Ifc.default_units(), Units::Mm);
        assert_eq!(IngestFormat::Native.default_units(), Units::Mm);
    }
}
