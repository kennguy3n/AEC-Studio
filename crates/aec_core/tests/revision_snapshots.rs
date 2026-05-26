//! Integration tests for filesystem-level revision snapshots and the
//! diff that compares two snapshots.
//!
//! These tests exercise the full path that the bridge's
//! `deliverCreateRevision` / `deliverCompareRevisions` endpoints will
//! take in production:
//!
//! 1. Open a real SQLCipher-encrypted project database via
//!    [`aec_core::db::open_encrypted`].
//! 2. Insert real `entities` rows simulating walls / sheets /
//!    schedule rows.
//! 3. Snapshot the project via
//!    [`aec_core::RevisionStore::create_with_snapshot`].
//! 4. Reopen the snapshot file with the same key and diff against a
//!    second snapshot using [`aec_core::compare_revision_snapshots`].
//!
//! No mocks. No stubs. Snapshots are real `.snap` files on disk,
//! diffs are real BLAKE3-hashed entity comparisons. The test thus
//! validates the on-disk shape, the encryption round-trip, and the
//! diff arithmetic in one shot.

use std::path::Path;

use aec_core::crypto::{derive_project_key, generate_project_nonce, Key32};
use aec_core::db::open_encrypted;
use aec_core::revision::{RevisionDraft, RevisionStore};
use aec_core::types::ProjectId;
use aec_core::version_diff::{
    classify_entity_kind, compare_revision_snapshots, snapshot_entities, EntityChangeKind,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn project_key() -> Key32 {
    let master = [42u8; 32];
    let nonce = generate_project_nonce().unwrap();
    derive_project_key(&master, &nonce)
}

fn open_project(td: &Path) -> (Connection, std::path::PathBuf, Key32) {
    let db_path = td.join("project.sqlite");
    let key = project_key();
    let conn = open_encrypted(&db_path, &key).unwrap();
    (conn, db_path, key)
}

fn insert_entity(conn: &Connection, id: &str, kind: &str, body: &str) {
    conn.execute(
        "INSERT INTO entities (id, kind, parent_id, created_at, updated_at, body)
         VALUES (?1, ?2, NULL, '2026-05-25T00:00:00Z', '2026-05-25T00:00:00Z', ?3)",
        rusqlite::params![id, kind, body],
    )
    .unwrap();
}

fn update_entity(conn: &Connection, id: &str, body: &str) {
    let n = conn
        .execute(
            "UPDATE entities SET body = ?2, updated_at = '2026-05-25T00:00:01Z' WHERE id = ?1",
            rusqlite::params![id, body],
        )
        .unwrap();
    assert_eq!(n, 1, "expected to update one row for id={id}");
}

fn delete_entity(conn: &Connection, id: &str) {
    let n = conn
        .execute("DELETE FROM entities WHERE id = ?1", rusqlite::params![id])
        .unwrap();
    assert_eq!(n, 1, "expected to delete one row for id={id}");
}

fn draft(tag: &str) -> RevisionDraft {
    RevisionDraft::new(
        ProjectId::new(),
        tag,
        format!("snapshot test {tag}"),
        "0000000000000000000000000000000000000000000000000000000000000000",
        "Test Project",
        "0.1.0",
    )
}

#[test]
fn snapshot_round_trip_preserves_entities_under_sqlcipher_key() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, key) = open_project(td.path());

    insert_entity(&conn, "wall.A1", "wall", r#"{"length_mm":4000}"#);
    insert_entity(&conn, "wall.A2", "wall", r#"{"length_mm":3000}"#);
    insert_entity(&conn, "room.LR", "room", r#"{"name":"Living"}"#);
    insert_entity(&conn, "sheet.A101", "sheet", r#"{"name":"A101"}"#);
    insert_entity(
        &conn,
        "schedule.row.D1",
        "schedule_row",
        r#"{"door":"D1","width":900}"#,
    );

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let rev = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    let snap = rev.snapshot.as_ref().expect("snapshot recorded");

    // Reopen the snapshot file with the same key and confirm every
    // entity round-tripped through the byte-for-byte copy.
    //
    // We open via `open_readonly` rather than `open_existing` because
    // production code (`version_diff::open_snapshot_db`) opens `.snap`
    // files strictly read-only via `SQLITE_OPEN_READ_ONLY` — that's
    // the whole point of the function. If this test opened with
    // `open_existing` (which uses default read/write flags + applies
    // `journal_mode = WAL`), SQLite would create `-wal`/`-shm`
    // sidecar files next to the `.snap`, which is exactly the
    // failure mode `open_readonly` was added to prevent. The test
    // should exercise the same code path as production.
    let snap_path = store.snapshot_path(&rev).unwrap();
    let snap_conn = aec_core::db::open_readonly(&snap_path, &key).unwrap();
    let entities = snapshot_entities(&snap_conn).unwrap();

    assert_eq!(entities.len(), 5);
    assert!(entities
        .iter()
        .any(|e| e.id == "wall.A1" && e.category == "geometry"));
    assert!(entities
        .iter()
        .any(|e| e.id == "sheet.A101" && e.category == "sheet"));
    assert!(entities
        .iter()
        .any(|e| e.id == "schedule.row.D1" && e.category == "schedule_row"));

    // BLAKE3 in the metadata still matches the on-disk file.
    assert!(store.verify_snapshot(&rev).unwrap());
    assert!(snap.size_bytes > 0);
}

#[test]
fn compare_revision_snapshots_reports_geometry_sheet_and_schedule_changes() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, key) = open_project(td.path());

    // Seed v1: 2 walls, 1 sheet, 1 schedule row.
    insert_entity(&conn, "wall.A", "wall", r#"{"length_mm":4000}"#);
    insert_entity(&conn, "wall.B", "wall", r#"{"length_mm":3000}"#);
    insert_entity(&conn, "sheet.A101", "sheet", r#"{"name":"A101"}"#);
    insert_entity(
        &conn,
        "sched.D1",
        "schedule_row",
        r#"{"door":"D1","width":900}"#,
    );

    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();

    // Mutate the live db towards v2:
    //   * wall.A: modified (length changed) → geometry.modified += 1
    //   * wall.B: removed                    → geometry.removed  += 1
    //   * wall.C: added                      → geometry.added    += 1
    //   * sheet.A102: added                  → sheet.added       += 1
    //   * sched.D1: modified (width changed) → schedule_row.modified += 1
    //   * sched.W1: added                    → schedule_row.added    += 1
    update_entity(&conn, "wall.A", r#"{"length_mm":4500}"#);
    delete_entity(&conn, "wall.B");
    insert_entity(&conn, "wall.C", "wall", r#"{"length_mm":3500}"#);
    insert_entity(&conn, "sheet.A102", "sheet", r#"{"name":"A102"}"#);
    update_entity(&conn, "sched.D1", r#"{"door":"D1","width":1000}"#);
    insert_entity(
        &conn,
        "sched.W1",
        "schedule_row",
        r#"{"window":"W1","width":1200}"#,
    );

    let v2 = store
        .create_with_snapshot(draft("v2"), &conn, &db_path)
        .unwrap();

    let diff = compare_revision_snapshots(&store, &v1, &v2, &key).unwrap();
    assert_eq!(diff.base_revision_id, v1.id);
    assert_eq!(diff.head_revision_id, v2.id);
    assert!(!diff.is_clean());

    let geom = diff.by_category.get("geometry").expect("geometry bucket");
    assert_eq!(geom.added, 1, "expected wall.C as the geometry addition");
    assert_eq!(
        geom.modified, 1,
        "expected wall.A as the geometry modification"
    );
    assert_eq!(geom.removed, 1, "expected wall.B as the geometry removal");

    let sheet = diff.by_category.get("sheet").expect("sheet bucket");
    assert_eq!(sheet.added, 1);
    assert_eq!(sheet.unchanged, 1, "sheet.A101 stayed identical");

    let sched = diff
        .by_category
        .get("schedule_row")
        .expect("schedule_row bucket");
    assert_eq!(sched.added, 1, "expected sched.W1 to be added");
    assert_eq!(sched.modified, 1, "expected sched.D1 to be modified");

    // Total changes across categories.
    assert_eq!(diff.total_changes(), 6);

    // Spot-check that the EntityChange entries carry both hashes for
    // modified entities (so the renderer can render before/after).
    let modified_wall = diff
        .changes
        .iter()
        .find(|c| c.id == "wall.A" && c.kind == EntityChangeKind::Modified)
        .expect("modified wall.A change entry");
    assert!(modified_wall.before_hash.is_some());
    assert!(modified_wall.after_hash.is_some());
    assert_ne!(modified_wall.before_hash, modified_wall.after_hash);
}

#[test]
fn compare_revision_snapshots_on_identical_state_is_clean() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, key) = open_project(td.path());
    insert_entity(&conn, "wall.A", "wall", r#"{"length_mm":4000}"#);
    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    let v2 = store
        .create_with_snapshot(draft("v2"), &conn, &db_path)
        .unwrap();
    let diff = compare_revision_snapshots(&store, &v1, &v2, &key).unwrap();
    assert!(diff.is_clean());
    assert_eq!(diff.total_changes(), 0);
}

#[test]
fn compare_revision_snapshots_errors_when_snapshot_file_missing() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, key) = open_project(td.path());
    insert_entity(&conn, "wall.A", "wall", r#"{}"#);
    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    let v2 = store
        .create_with_snapshot(draft("v2"), &conn, &db_path)
        .unwrap();

    // Tamper: delete v2's snapshot file out from under the store.
    let v2_snap = store.snapshot_path(&v2).unwrap();
    std::fs::remove_file(&v2_snap).unwrap();

    let err = compare_revision_snapshots(&store, &v1, &v2, &key).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("snapshot file missing"),
        "expected missing-snapshot error, got: {msg}"
    );
}

#[test]
fn compare_revision_snapshots_errors_when_revision_has_no_snapshot() {
    let td = TempDir::new().unwrap();
    let (conn, db_path, _key) = open_project(td.path());
    insert_entity(&conn, "wall.A", "wall", r#"{}"#);
    let store = RevisionStore::open(td.path().join("revisions")).unwrap();
    // v1 has a snapshot.
    let v1 = store
        .create_with_snapshot(draft("v1"), &conn, &db_path)
        .unwrap();
    // v2 is created via the legacy `create` path — no snapshot.
    let v2 = store.create(draft("v2")).unwrap();
    assert!(v2.snapshot.is_none());

    let err = compare_revision_snapshots(&store, &v1, &v2, &project_key()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no snapshot"),
        "expected missing-snapshot error, got: {msg}"
    );
}

#[test]
fn classify_entity_kind_buckets_known_kinds_into_three_levels() {
    assert_eq!(classify_entity_kind("wall"), "geometry");
    assert_eq!(classify_entity_kind("floor"), "geometry");
    assert_eq!(classify_entity_kind("ceiling"), "geometry");
    assert_eq!(classify_entity_kind("room"), "geometry");
    assert_eq!(classify_entity_kind("door"), "geometry");
    assert_eq!(classify_entity_kind("polyline"), "geometry");
    assert_eq!(classify_entity_kind("furniture"), "geometry");
    assert_eq!(classify_entity_kind("dimension"), "geometry");

    assert_eq!(classify_entity_kind("sheet"), "sheet");
    assert_eq!(classify_entity_kind("viewport"), "sheet");
    assert_eq!(classify_entity_kind("title_block"), "sheet");

    assert_eq!(classify_entity_kind("schedule_row"), "schedule_row");
    assert_eq!(classify_entity_kind("schedule_column"), "schedule_row");

    // Pass-through for non-bucketed kinds.
    assert_eq!(classify_entity_kind("camera"), "camera");
    assert_eq!(classify_entity_kind("material"), "material");
    assert_eq!(classify_entity_kind("lighting"), "lighting");
}

#[test]
fn snapshot_entities_hashes_distinguish_kind_changes_from_body_changes() {
    let td = TempDir::new().unwrap();
    let (conn, _db_path, _key) = open_project(td.path());

    // Same id, same body, different kind → must hash differently.
    insert_entity(&conn, "ent.A", "wall", r#"{}"#);
    let snapshot_a = snapshot_entities(&conn).unwrap();

    conn.execute("UPDATE entities SET kind = 'floor' WHERE id = 'ent.A'", [])
        .unwrap();
    let snapshot_b = snapshot_entities(&conn).unwrap();

    assert_ne!(
        snapshot_a[0].payload_hash, snapshot_b[0].payload_hash,
        "kind change must invalidate the payload hash"
    );
    // And the category bucket flips too.
    assert_eq!(snapshot_a[0].category, "geometry");
    assert_eq!(snapshot_b[0].category, "geometry");

    // Body change with same kind also flips the hash.
    conn.execute(
        r#"UPDATE entities SET body = '{"length_mm":5000}' WHERE id = 'ent.A'"#,
        [],
    )
    .unwrap();
    let snapshot_c = snapshot_entities(&conn).unwrap();
    assert_ne!(snapshot_b[0].payload_hash, snapshot_c[0].payload_hash);
}

#[test]
fn snapshot_entities_hash_includes_parent_id_so_reparenting_is_detected() {
    let td = TempDir::new().unwrap();
    let (conn, _db_path, _key) = open_project(td.path());

    // Seed two rooms and a wall whose parent is the first room.
    insert_entity(&conn, "room.kitchen", "room", r#"{"area":12}"#);
    insert_entity(&conn, "room.living", "room", r#"{"area":24}"#);
    conn.execute(
        "INSERT INTO entities (id, kind, parent_id, created_at, updated_at, body)
         VALUES ('wall.W1', 'wall', 'room.kitchen', '2026-05-25T00:00:00Z', '2026-05-25T00:00:00Z', '{}')",
        [],
    )
    .unwrap();
    let snap_before = snapshot_entities(&conn).unwrap();
    let wall_before = snap_before
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 in baseline");

    // Re-parent the wall under the other room. The body bytes are
    // unchanged, but the topology has changed — the diff engine MUST
    // see this as a modification (otherwise re-parenting silently
    // disappears from the before/after report).
    conn.execute(
        "UPDATE entities SET parent_id = 'room.living' WHERE id = 'wall.W1'",
        [],
    )
    .unwrap();
    let snap_after = snapshot_entities(&conn).unwrap();
    let wall_after = snap_after
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 still present");

    assert_ne!(
        wall_before.payload_hash, wall_after.payload_hash,
        "re-parenting must invalidate the payload hash"
    );
}

#[test]
fn snapshot_entities_hash_includes_components_so_component_only_edits_are_detected() {
    let td = TempDir::new().unwrap();
    let (conn, _db_path, _key) = open_project(td.path());

    // Seed an entity with one component attached.
    insert_entity(&conn, "wall.W1", "wall", r#"{"length_mm":4000}"#);
    conn.execute(
        "INSERT INTO components (id, entity_id, kind, body)
         VALUES ('comp.geom.1', 'wall.W1', 'geometry', '{\"thickness_mm\":100}')",
        [],
    )
    .unwrap();
    let snap_before = snapshot_entities(&conn).unwrap();
    let wall_before = snap_before
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 in baseline");

    // Mutate ONLY the component body. The entity row is untouched.
    // Without component-aware hashing this would look identical and
    // the diff UI would lie to the user.
    conn.execute(
        "UPDATE components SET body = '{\"thickness_mm\":150}' WHERE id = 'comp.geom.1'",
        [],
    )
    .unwrap();
    let snap_after_edit = snapshot_entities(&conn).unwrap();
    let wall_after_edit = snap_after_edit
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 still present");
    assert_ne!(
        wall_before.payload_hash, wall_after_edit.payload_hash,
        "editing a component body must invalidate the parent entity's payload hash"
    );

    // Adding a new component to the entity is also a modification.
    conn.execute(
        "INSERT INTO components (id, entity_id, kind, body)
         VALUES ('comp.mat.1', 'wall.W1', 'material', '{\"name\":\"oak\"}')",
        [],
    )
    .unwrap();
    let snap_after_add = snapshot_entities(&conn).unwrap();
    let wall_after_add = snap_after_add
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 still present");
    assert_ne!(
        wall_after_edit.payload_hash, wall_after_add.payload_hash,
        "attaching a new component must invalidate the parent entity's payload hash"
    );

    // Removing a component reverts the hash to a different state too.
    conn.execute("DELETE FROM components WHERE id = 'comp.geom.1'", [])
        .unwrap();
    let snap_after_remove = snapshot_entities(&conn).unwrap();
    let wall_after_remove = snap_after_remove
        .iter()
        .find(|e| e.id == "wall.W1")
        .expect("wall.W1 still present");
    assert_ne!(
        wall_after_add.payload_hash, wall_after_remove.payload_hash,
        "removing a component must invalidate the parent entity's payload hash"
    );
}
