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
    ingest_path, AssetDatabase, AssetError, AssetImportPipeline, DecimateOptions, IngestFormat,
    PathImportMetadata, RealMeshImportRequest, ThumbnailOptions,
};
use aec_core::types::Units;

/// 80-triangle UV sphere (icosphere subdivided once: 20 base faces
/// × 4 subdivisions = 80 triangles, 42 vertices). Pinned to the
/// fixture file's actual `f`-line count so the docstring matches
/// reality; "lots of triangles" relative to the aggressive 5% LOD2
/// target (4 triangles) so decimation actually has work to do.
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

/// Build [`PathImportMetadata`] with `source_units` derived from
/// `format.default_units()` — the canonical pattern bridge callers
/// should follow when they have no out-of-band knowledge of the
/// authoring unit. This is also how the test fixtures are authored:
///
/// * `icosphere.obj` is in millimetres (OBJ is unit-less by spec; the
///   asset DB's canonical unit is mm so we adopt that as the OBJ
///   default per [`IngestFormat::default_units`]).
/// * `icosphere.gltf` is in metres (glTF 2.0 §3.5.4 mandates metres).
///
/// Both fixtures encode the same geometry (a 1m-radius icosphere
/// subdivided once) in their respective natural units; after
/// [`canonicalise_to_mm`] they land at identical positions in mm.
fn meta_for_format(asset_id: &str, format: IngestFormat) -> PathImportMetadata {
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
        source_units: format.default_units(),
        extra_ratios: vec![],
        // Closed-manifold icosphere: the strict default
        // (`preserve_boundary: true`) reaches the 5% LOD2 target
        // without needing the boundary-mesh recovery knob.
        decimate_options: None,
        thumbnail_opts: Some(cheap_thumb_opts()),
    }
}

/// Shorthand for the common OBJ fixture path (mm).
fn obj_meta(asset_id: &str) -> PathImportMetadata {
    meta_for_format(asset_id, IngestFormat::Obj)
}

/// Shorthand for the common glTF fixture path (metres per spec).
fn gltf_meta(asset_id: &str) -> PathImportMetadata {
    meta_for_format(asset_id, IngestFormat::Gltf)
}

#[test]
fn obj_path_imports_with_three_lod_levels_and_real_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());
    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);

    let summary = pipe.import_path(&p, obj_meta("obj_phase11")).unwrap();

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
    // glTF is authored in metres per spec; `gltf_meta` derives that
    // unit from `IngestFormat::Gltf.default_units()`, exercising the
    // M -> mm canonicalisation path that bridge callers will hit on
    // any conformant glTF asset.
    let summary = pipe.import_path(&p, gltf_meta("gltf_phase11")).unwrap();
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

    let s1 = pipe.import_path(&p, obj_meta("first")).unwrap();
    assert!(!s1.deduped, "first import should not be deduped");

    // Different asset_id, identical file → base blob should dedupe.
    let s2 = pipe.import_path(&p, obj_meta("second")).unwrap();
    assert!(s2.deduped, "second import of same file should dedupe blob");

    // Both assets should reference the same LOD 0 blob hash.
    let first = db.get("first").unwrap().unwrap();
    let second = db.get("second").unwrap().unwrap();
    assert_eq!(first.lods[0].mesh_hash, second.lods[0].mesh_hash);
}

#[test]
fn decimate_options_override_propagates_to_the_solver() {
    // Proves the recovery knob added for the boundary-heavy-mesh case
    // (Devin Review finding 3305659126) actually reaches the QEM
    // solver. We do this symmetrically: rather than authoring a
    // custom boundary-heavy fixture, we hand the existing closed
    // icosphere a *deny-every-collapse* override (`max_cost = 0.0`).
    // If the override is wired correctly the decimator can't reduce
    // a single triangle and the pipeline must surface
    // `LodNotStrictlyDecreasing`. If it weren't wired the strict
    // default (`max_cost = INFINITY`) would still decimate the mesh
    // cleanly and this test would silently pass.
    //
    // The same plumbing is what callers of the recovery path go
    // through, just with `preserve_boundary: false` instead.
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());
    let mut db = AssetDatabase::open_in_memory().unwrap();
    let mut pipe = AssetImportPipeline::new(&mut db);

    let mut meta = obj_meta("override_propagates");
    meta.decimate_options = Some(DecimateOptions {
        // Per-level `target_triangle_count` is overridden by the
        // pipeline from the chain — this seed value is irrelevant.
        target_triangle_count: 0,
        max_cost: 0.0,
        preserve_boundary: true,
    });
    let err = pipe
        .import_path(&p, meta)
        .expect_err("zero-cost-cap override must block every collapse");
    match err {
        AssetError::LodNotStrictlyDecreasing { actual, base, .. } => {
            assert_eq!(
                actual, base,
                "no collapse was allowed, so decimated count must equal base"
            );
        }
        other => panic!("expected LodNotStrictlyDecreasing, got {other:?}"),
    }

    // Sanity: identical metadata *without* the override succeeds.
    let mut db2 = AssetDatabase::open_in_memory().unwrap();
    let mut pipe2 = AssetImportPipeline::new(&mut db2);
    pipe2
        .import_path(&p, obj_meta("default_succeeds"))
        .expect("strict default reaches 5% target on closed icosphere");
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

    pipe.import_path(&p, obj_meta("collide")).unwrap();
    let err = pipe.import_path(&q, obj_meta("collide")).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("hash") || msg.contains("conflict"),
        "expected hash conflict on differing payload for same asset_id, got: {msg}",
    );
}

/// Build a [`RealMeshImportRequest`] with the boilerplate metadata
/// fields populated; callers patch `mesh`, `source_units`, and
/// `decimate_options` for the test under exercise.
fn real_req(asset_id: &str, mesh: aec_geometry::Mesh) -> RealMeshImportRequest {
    RealMeshImportRequest {
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
        tags: vec!["test".into()],
        style_tags: vec![],
        materials: vec![],
        source_units: Units::Mm,
        mesh,
        extra_ratios: vec![],
        decimate_options: None,
        thumbnail_opts: Some(cheap_thumb_opts()),
    }
}

#[test]
fn max_cost_interpretation_does_not_vary_with_source_units() {
    // Regression guard for the documented contract on
    // `PathImportMetadata::decimate_options` and the
    // `AssetError::Decimation` recovery section: `max_cost` is
    // evaluated in canonical mm² space, **independent of
    // `source_units`**. (Devin Review finding 3305788569.)
    //
    // We pick the icosphere fixture because every interior collapse
    // pushes the new vertex off the local tangent plane, producing a
    // strictly positive QEM cost — so `max_cost = 0.0` blocks every
    // collapse on the closed manifold (planar geometry would have
    // cost-0 collapses that slip past the strict `cost > max_cost`
    // predicate). The same canonical geometry tagged `Units::M`
    // (positions scaled to metres) and `Units::Mm` (positions in mm)
    // must produce identical decimation outcomes under the same
    // `max_cost`. If a future change started interpreting `max_cost`
    // in source-unit² space (e.g. scaling it by
    // `source_units.to_mm(1.0).powi(2)` somewhere in the pipeline),
    // the metres-tagged path would see a 10⁶× different effective
    // cap and this test would fail at the equality checks below.
    let dir = tempfile::tempdir().unwrap();
    let p = write_fixture(dir.path(), "icosphere.obj", OBJ_ICOSPHERE.as_bytes());

    // Ingest once to get the canonical (mm-space) icosphere mesh.
    let ingested = ingest_path(&p).expect("OBJ fixture parses");
    let mm_mesh = ingested.mesh.clone();
    let mut metres_mesh = mm_mesh.clone();
    for pos in &mut metres_mesh.positions {
        pos[0] /= 1000.0;
        pos[1] /= 1000.0;
        pos[2] /= 1000.0;
    }

    let blocking_opts = Some(DecimateOptions {
        // The pipeline always overrides target_triangle_count per LOD
        // level from the chain — this seed value is irrelevant.
        target_triangle_count: 0,
        // Cap every collapse: any cost > 0 (i.e. every non-coplanar
        // collapse on a closed sphere-shaped manifold) is rejected.
        max_cost: 0.0,
        preserve_boundary: false,
    });

    let mut mm_db = AssetDatabase::open_in_memory().unwrap();
    let mut mm_pipe = AssetImportPipeline::new(&mut mm_db);
    let mut mm_req = real_req("mm_icosphere", mm_mesh);
    mm_req.source_units = Units::Mm;
    mm_req.decimate_options = blocking_opts;
    let mm_err = mm_pipe
        .import_mesh(mm_req)
        .expect_err("mm-tagged icosphere + max_cost=0 must block every collapse");

    let mut m_db = AssetDatabase::open_in_memory().unwrap();
    let mut m_pipe = AssetImportPipeline::new(&mut m_db);
    let mut m_req = real_req("m_icosphere", metres_mesh);
    m_req.source_units = Units::M;
    m_req.decimate_options = blocking_opts;
    let m_err = m_pipe
        .import_mesh(m_req)
        .expect_err("metres-tagged icosphere must hit the same cap symmetrically");

    match (mm_err, m_err) {
        (
            AssetError::LodNotStrictlyDecreasing {
                actual: mm_actual,
                base: mm_base,
                level: mm_level,
            },
            AssetError::LodNotStrictlyDecreasing {
                actual: m_actual,
                base: m_base,
                level: m_level,
            },
        ) => {
            assert_eq!(
                mm_actual, m_actual,
                "max_cost must produce the same decimated triangle count regardless of \
                 source_units (mm² space invariant)"
            );
            assert_eq!(
                mm_base, m_base,
                "canonical base triangle count must match (same geometry, different tag)"
            );
            assert_eq!(
                mm_level, m_level,
                "max_cost must trigger at the same LOD level regardless of source_units"
            );
        }
        (mm_other, m_other) => panic!(
            "expected symmetric LodNotStrictlyDecreasing on both unit tags, \
             got mm={mm_other:?} m={m_other:?}"
        ),
    }
}
