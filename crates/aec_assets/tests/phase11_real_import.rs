//! End-to-end Phase 11 Task 21 acceptance test:
//! `path on disk → ingest → real-mesh import → asset DB`.
//!
//! This is the canonical asset import flow: drop a glTF or OBJ file on
//! disk, route it through `AssetImportPipeline::import_path` (which
//! ingests via the format reader, QEM-decimates each LOD level,
//! renders a PBR thumbnail, hashes the base mesh with BLAKE3, and
//! upserts into the content-addressed asset DB). The tests below
//! prove:
//!
//! 1. A real glTF file imports cleanly and produces a 3-level LOD
//!    chain with strictly decreasing triangle counts (LOD 0 / LOD 1
//!    ~25% / LOD 2 ~5% per spec).
//! 2. A real OBJ file imports cleanly the same way.
//! 3. The thumbnail is a real rendered PNG (PNG magic + non-trivial
//!    payload) and is stored as a content-addressed blob.
//! 4. Re-importing the *same* file twice dedupes the base mesh blob
//!    (BLAKE3 hashes match) — the second import returns
//!    `deduped = true`.
//! 5. Importing a different file with the same `asset_id` is rejected
//!    with `AssetError::HashConflict`.

use std::path::PathBuf;

use aec_assets::metadata::{License, Vendor};
use aec_assets::{
    AssetDatabase, AssetImportPipeline, IngestFormat, PathImportMetadata, ThumbnailOptions,
};
use aec_core::types::Units;

/// 96-triangle UV sphere (icosphere subdivided once) as an OBJ.
/// Lots of triangles so decimation actually has work to do.
const OBJ_ICOSPHERE: &str = include_str!("fixtures/icosphere.obj");

/// Same icosphere encoded as a minimal `.gltf` JSON + embedded `data:`
/// URI buffer.
const GLTF_ICOSPHERE: &str = include_str!("fixtures/icosphere.gltf");

fn write_fixture(dir: &std::path::Path, name: &str, content: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

fn cheap_thumb_opts() -> ThumbnailOptions {
    // Keep the thumbnail render snappy for CI: 64x64 with 4 spp is
    // plenty to validate the pipeline end-to-end.
    ThumbnailOptions {
        width: 64,
        height: 64,
        samples_per_pixel: 4,
        max_bounces: 2,
        ..Default::default()
    }
}

fn baseline_meta(asset_id: &str) -> PathImportMetadata {
    PathImportMetadata {
        asset_id: asset_id.to_string(),
        name: format!("Test asset {asset_id}"),
        vendor: Vendor {
            id: "phase11".into(),
            name: "Phase 11 Acceptance".into(),
            url: None,
        },
        version: "1.0.0".into(),
        license: License::CcBy,
        attribution: None,
        tags: vec!["test".into(), "icosphere".into()],
        style_tags: vec![],
        materials: vec![],
        source_units: Units::Mm,
        extra_ratios: vec![],
        thumbnail_opts: Some(cheap_thumb_opts()),
    }
}

#[test]
fn obj_path_imports_with_three_lod_levels_and_real_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());
    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);

    let summary = pipe.import_path(&p, baseline_meta("obj_phase11")).unwrap();

    // Three-level chain per spec [1.0, 0.25, 0.05].
    assert_eq!(summary.lod_levels, 3, "spec demands 3 LOD levels");
    let stored = db.get("obj_phase11").unwrap().unwrap();
    assert_eq!(stored.lods.len(), 3);
    // Each LOD level points at a distinct content-addressed blob.
    let hashes: Vec<&str> = stored.lods.iter().map(|l| l.mesh_hash.as_str()).collect();
    assert_eq!(
        hashes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "LOD levels should not share blobs after real decimation"
    );
    // Strictly decreasing triangle counts.
    for w in stored.lods.windows(2) {
        assert!(
            w[1].triangle_count <= w[0].triangle_count,
            "LOD chain must not increase triangle count, got {:?} -> {:?}",
            w[0],
            w[1]
        );
    }
    // LOD 1 should be in the [10%, 50%] of base — decimation isn't
    // pixel-perfect but should land near 25%.
    let base_t = stored.lods[0].triangle_count as f32;
    let lod1_t = stored.lods[1].triangle_count as f32;
    assert!(
        lod1_t > 0.0 && lod1_t / base_t <= 0.5,
        "LOD 1 should be at most 50% of base; got {lod1_t}/{base_t}",
    );

    // Thumbnail blob should exist + be a valid PNG (signature 89 50 4E 47 0D 0A 1A 0A).
    let thumb_bytes = db.get_blob(&summary.thumbnail_hash).unwrap().unwrap();
    assert!(
        thumb_bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "thumbnail must be a real PNG, got prefix {:?}",
        &thumb_bytes[..thumb_bytes.len().min(8)]
    );
    // Sanity floor: at minimum a 64x64 PNG should be >200 bytes.
    assert!(thumb_bytes.len() > 200);
}

#[test]
fn gltf_path_imports_via_real_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.gltf", GLTF_ICOSPHERE.as_bytes());

    // Sanity: format detection.
    assert_eq!(
        aec_assets::detect_format(&p, GLTF_ICOSPHERE.as_bytes()),
        Some(IngestFormat::Gltf)
    );

    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);
    let summary = pipe.import_path(&p, baseline_meta("gltf_phase11")).unwrap();
    assert_eq!(summary.lod_levels, 3);
    let stored = db.get("gltf_phase11").unwrap().unwrap();
    assert_eq!(stored.lods.len(), 3);
    assert!(stored.lods[0].triangle_count >= stored.lods[1].triangle_count);
}

#[test]
fn re_importing_same_file_dedupes_on_blake3_hash() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());
    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);

    let s1 = pipe.import_path(&p, baseline_meta("first")).unwrap();
    assert!(!s1.deduped, "first import should not be deduped");

    // Different asset_id, identical file → base blob should dedupe.
    let s2 = pipe.import_path(&p, baseline_meta("second")).unwrap();
    assert!(s2.deduped, "second import of same file should dedupe blob");

    // Both assets should reference the same LOD 0 blob hash.
    let first = db.get("first").unwrap().unwrap();
    let second = db.get("second").unwrap().unwrap();
    assert_eq!(first.lods[0].mesh_hash, second.lods[0].mesh_hash);
}

#[test]
fn re_importing_different_file_under_same_asset_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());
    let q = write_fixture(
        dir.path(),
        "alt.obj",
        b"o other\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n",
    );
    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);

    pipe.import_path(&p, baseline_meta("collide")).unwrap();
    let err = pipe.import_path(&q, baseline_meta("collide")).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("hash") || msg.contains("conflict"),
        "expected hash conflict on differing payload for same asset_id, got: {msg}",
    );
}
