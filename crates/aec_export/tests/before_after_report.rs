//! End-to-end tests for [`aec_export::before_after::BeforeAfterReport`].
//!
//! Exercises the full pipeline:
//!
//! 1. Open a real SQLCipher project DB.
//! 2. Insert wall / floor / room entities with realistic JSON bodies.
//! 3. Take revision v1 via `RevisionStore::create_with_snapshot`.
//! 4. Mutate (add wall, remove wall, move wall).
//! 5. Take revision v2.
//! 6. Compute `BeforeAfterReport`.
//! 7. Assert classification counts + SVG output + render-pair
//!    discovery against the on-disk `renders/` directory.
//!
//! No mocks. The .snap files are real SQLCipher-encrypted SQLite
//! databases on disk; the SVG is run through a minimal element-count
//! check.

use aec_core::crypto::{derive_project_key, generate_project_nonce, Key32};
use aec_core::db::open_encrypted;
use aec_core::revision::{RevisionDraft, RevisionStore};
use aec_core::types::ProjectId;
use aec_export::before_after::{BeforeAfterReport, BeforeAfterReportError, PlanOverlayLevel};
use rusqlite::Connection;
use tempfile::TempDir;

fn key() -> Key32 {
    let master = [9u8; 32];
    let nonce = generate_project_nonce().unwrap();
    derive_project_key(&master, &nonce)
}

fn open_project(td: &std::path::Path) -> (Connection, std::path::PathBuf, Key32) {
    let db = td.join("project.sqlite");
    let k = key();
    let c = open_encrypted(&db, &k).unwrap();
    (c, db, k)
}

fn insert_wall(conn: &Connection, id: &str, start: [f64; 2], end: [f64; 2], thickness: f64) {
    let body = serde_json::json!({
        "entity_id": id,
        "start_mm": start,
        "end_mm": end,
        "thickness_mm": thickness,
        "height_mm": 2700.0,
    });
    conn.execute(
        "INSERT INTO entities (id, kind, parent_id, created_at, updated_at, body)
         VALUES (?1, 'wall', NULL, '2026-05-25T00:00:00Z', '2026-05-25T00:00:00Z', ?2)",
        rusqlite::params![id, body.to_string()],
    )
    .unwrap();
}

fn update_wall_body(conn: &Connection, id: &str, body: serde_json::Value) {
    let n = conn
        .execute(
            "UPDATE entities SET body = ?2, updated_at = '2026-05-25T01:00:00Z' WHERE id = ?1",
            rusqlite::params![id, body.to_string()],
        )
        .unwrap();
    assert_eq!(n, 1);
}

fn delete_entity(conn: &Connection, id: &str) {
    let n = conn
        .execute("DELETE FROM entities WHERE id = ?1", rusqlite::params![id])
        .unwrap();
    assert_eq!(n, 1);
}

fn draft(tag: &str) -> RevisionDraft {
    RevisionDraft::new(
        ProjectId::new(),
        tag,
        format!("test {tag}"),
        "0000000000000000000000000000000000000000000000000000000000000000",
        "Test Renovation",
        "0.1.0",
    )
}

#[test]
fn before_after_report_end_to_end_against_real_sqlcipher_snapshots() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, k) = open_project(td.path());

    // Base: 4-sided rectangular room.
    insert_wall(&conn, "w.south", [0.0, 0.0], [4000.0, 0.0], 200.0);
    insert_wall(&conn, "w.west", [0.0, 0.0], [0.0, 3000.0], 200.0);
    insert_wall(&conn, "w.north", [0.0, 3000.0], [4000.0, 3000.0], 200.0);
    insert_wall(&conn, "w.east", [4000.0, 0.0], [4000.0, 3000.0], 200.0);

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1-base"), &conn, &db_path)
        .unwrap();

    // Renovation: remove the west wall (knock through), extend the
    // south wall to 4500mm, add a new partition wall.
    delete_entity(&conn, "w.west");
    update_wall_body(
        &conn,
        "w.south",
        serde_json::json!({
            "entity_id": "w.south",
            "start_mm": [0.0, 0.0],
            "end_mm": [4500.0, 0.0],
            "thickness_mm": 200.0,
            "height_mm": 2700.0,
        }),
    );
    insert_wall(&conn, "w.partition", [2000.0, 0.0], [2000.0, 3000.0], 100.0);

    let v2 = store
        .create_with_snapshot(draft("v2-head"), &conn, &db_path)
        .unwrap();

    let report = BeforeAfterReport::compute("Test Renovation", &store, &v1, &v2, &k).unwrap();

    assert_eq!(report.project_name, "Test Renovation");
    assert_eq!(report.base_revision_id, v1.id);
    assert_eq!(report.head_revision_id, v2.id);
    assert_eq!(report.base_tag, "v1-base");
    assert_eq!(report.head_tag, "v2-head");

    let counts = report.plan_overlay.counts();
    assert_eq!(counts.unchanged, 2, "w.north and w.east kept exactly");
    assert_eq!(counts.modified, 1, "w.south length grew");
    assert_eq!(counts.new_construction, 1, "w.partition added");
    // w.west deleted + previous footprint of modified w.south = 2
    // demolition entries.
    assert_eq!(counts.demolition, 2);

    let bbox = report.plan_overlay.bbox_mm.expect("bbox set");
    assert_eq!(
        bbox,
        [0.0, 0.0, 4500.0, 3000.0],
        "bbox spans base + head walls"
    );

    // SVG output sanity checks.
    let svg = report.render_plan_overlay_svg(120.0).unwrap();
    assert!(svg.contains("<?xml"));
    assert!(svg.contains(r#"id="overlay-demolition""#));
    assert!(svg.contains(r#"id="overlay-new""#));
    assert!(svg.contains(r#"id="overlay-unchanged""#));
    assert!(svg.contains(r#"id="overlay-modified""#));
    // Red appears (demolition) and green appears (new).
    assert!(svg.contains(PlanOverlayLevel::Demolition.stroke()));
    assert!(svg.contains(PlanOverlayLevel::New.stroke()));

    // Round-trip the report through JSON.
    let json = serde_json::to_string(&report).unwrap();
    let restored: BeforeAfterReport = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, report);

    // Write the SVG to disk and verify its size.
    let svg_path = td.path().join("plan_overlay.svg");
    let written = report.write_plan_overlay_svg(&svg_path, 50.0).unwrap();
    assert_eq!(written, svg_path);
    let on_disk = std::fs::read_to_string(&svg_path).unwrap();
    assert!(on_disk.starts_with("<?xml"));
    assert!(on_disk.len() > 200);

    // Version diff bucket counts match what we mutated.
    let geom = report
        .version_diff
        .by_category
        .get("geometry")
        .expect("geometry diff bucket");
    assert_eq!(geom.added, 1);
    assert_eq!(geom.modified, 1);
    assert_eq!(geom.removed, 1);
}

#[test]
fn report_errors_when_revision_has_no_snapshot() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, k) = open_project(td.path());
    insert_wall(&conn, "w.A", [0.0, 0.0], [4000.0, 0.0], 200.0);

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    // v2 has no snapshot — via the legacy create path.
    let v2 = store.create(draft("v2")).unwrap();
    assert!(v2.snapshot.is_none());

    let err = BeforeAfterReport::compute("X", &store, &v1, &v2, &k).unwrap_err();
    assert!(matches!(err, BeforeAfterReportError::AecCore(_)));
    let msg = format!("{err}");
    assert!(msg.contains("no snapshot"), "got: {msg}");
}

#[test]
fn report_errors_on_malformed_wall_body() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, k) = open_project(td.path());
    // Wall with a malformed body (missing start_mm).
    conn.execute(
        "INSERT INTO entities (id, kind, parent_id, created_at, updated_at, body)
         VALUES ('w.bad', 'wall', NULL, '2026-05-25T00:00:00Z', '2026-05-25T00:00:00Z', ?1)",
        rusqlite::params![r#"{"end_mm":[1.0,2.0],"thickness_mm":100}"#],
    )
    .unwrap();

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    let v2 = store
        .create_with_snapshot(draft("v2"), &conn, &db_path)
        .unwrap();

    let err = BeforeAfterReport::compute("X", &store, &v1, &v2, &k).unwrap_err();
    assert!(matches!(err, BeforeAfterReportError::MalformedWall { .. }));
}

#[test]
fn discover_render_pairs_finds_matching_files_in_real_directory() {
    let td = TempDir::new().unwrap();
    let renders = td.path().join("renders");
    std::fs::create_dir_all(&renders).unwrap();

    let base_id = "rev_aaaaaaaa";
    let head_id = "rev_bbbbbbbb";

    std::fs::write(
        renders.join(format!("cam01__standard__{base_id}.png")),
        b"a",
    )
    .unwrap();
    std::fs::write(
        renders.join(format!("cam01__standard__{head_id}.png")),
        b"b",
    )
    .unwrap();
    std::fs::write(renders.join(format!("cam02__studio__{base_id}.png")), b"c").unwrap();
    std::fs::write(renders.join(format!("cam02__studio__{head_id}.png")), b"d").unwrap();
    // Lone file — only present in head; should not be paired.
    std::fs::write(renders.join(format!("cam03__draft__{head_id}.png")), b"e").unwrap();

    let pairs = BeforeAfterReport::discover_render_pairs(&renders, base_id, head_id);
    assert_eq!(pairs.len(), 2, "expected 2 complete pairs, got {pairs:?}");
    let labels: Vec<_> = pairs.iter().map(|p| p.label.clone()).collect();
    assert!(labels.contains(&"cam01 · standard".into()));
    assert!(labels.contains(&"cam02 · studio".into()));
}

#[test]
fn empty_project_produces_clean_report() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, k) = open_project(td.path());

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    let v2 = store
        .create_with_snapshot(draft("v2"), &conn, &db_path)
        .unwrap();

    let report = BeforeAfterReport::compute("Empty", &store, &v1, &v2, &k).unwrap();
    let counts = report.plan_overlay.counts();
    assert_eq!(counts.demolition, 0);
    assert_eq!(counts.new_construction, 0);
    assert_eq!(counts.modified, 0);
    assert_eq!(counts.unchanged, 0);
    assert!(report.plan_overlay.bbox_mm.is_none());

    let svg = report.render_plan_overlay_svg(50.0).unwrap();
    assert!(svg.contains("No walls in either revision"));
}
