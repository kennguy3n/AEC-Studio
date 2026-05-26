//! Tagged revision snapshots for the project package.
//!
//! A revision is a named, timestamped snapshot of the project's
//! deliverable surface — its manifest, the audit-chain head hash at the
//! moment of snapshot, and a list of arbitrary "tracked entity" entries
//! (geometry, sheets, schedule rows) that the higher crates fill in.
//!
//! Revisions live under `<project>/revisions/<revision-id>.json` and
//! are written atomically (write to `<id>.json.tmp`, rename). The
//! [`RevisionStore`] owns the directory and exposes
//! create/list/get/restore semantics.
//!
//! This module deliberately doesn't know about the contents of a
//! revision beyond strings: the `aec_command` / `aec_render` /
//! `aec_export` crates supply their own entity snapshots and feed them
//! into [`Revision::tracked_entities`]. Comparison logic lives in
//! [`crate::version_diff`].

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AecError;
use crate::types::ProjectId;

/// A single tracked entity captured in a revision. The `category` is
/// free-form ("geometry", "sheet", "schedule_row", "camera", ...) so the
/// store doesn't need to know every domain; the `payload_hash` lets the
/// diff engine spot whether two revisions changed the same entity
/// without re-comparing payloads field-by-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionEntity {
    /// Domain category, e.g. `"geometry"`, `"sheet"`, `"schedule_row"`.
    pub category: String,
    /// Domain-specific stable id within the category (entity id,
    /// sheet id, row key, …).
    pub id: String,
    /// BLAKE3 hex digest of the entity's canonical serialised form.
    /// Used by `version_diff` for cheap structural comparison.
    pub payload_hash: String,
    /// Optional human-readable label for UIs. Not part of the diff key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A complete tagged revision.
///
/// Serialised one-revision-per-file under `<project>/revisions/<id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub project_id: ProjectId,
    /// User-supplied tag name, e.g. `"client-review-2026-05-21"`. Must
    /// be unique within a project — [`RevisionStore::create`] rejects
    /// duplicates so revisions can be referenced by tag from the UI.
    pub tag: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    /// Hex BLAKE3 of the audit-chain head when the revision was taken.
    pub audit_chain_head: String,
    /// Snapshot of the project manifest version + name when revision
    /// was taken — useful for UI display without loading the manifest.
    pub manifest_name: String,
    pub manifest_app_version: String,
    /// Tracked entities captured in this revision.
    pub tracked_entities: Vec<RevisionEntity>,
    /// On-disk database snapshot. `None` for legacy revisions taken
    /// before [`RevisionStore::create_with_snapshot`] existed, in which
    /// case the only material the diff engine has to work with is the
    /// pre-computed `tracked_entities`. Populated revisions point at a
    /// `revisions/<id>.snap` file inside the project package — a byte-
    /// for-byte copy of the SQLCipher project database at the moment
    /// the revision was sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<RevisionSnapshot>,
}

/// Metadata describing a filesystem-level snapshot of the project
/// database that backs a [`Revision`]. The actual file lives at
/// `<project>/revisions/<relative_path>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionSnapshot {
    /// Path of the snapshot file, relative to the revisions directory.
    /// Always `"<revision-id>.snap"` for revisions created by
    /// [`RevisionStore::create_with_snapshot`]; stored as a relative
    /// path so a project package can be moved or renamed and the
    /// snapshot reference is still resolvable.
    pub relative_path: String,
    /// BLAKE3 hex digest of the snapshot file's bytes, computed at
    /// snapshot time. The store can verify the on-disk file still
    /// matches this digest via [`RevisionStore::verify_snapshot`] — a
    /// mismatch means the file was tampered with after the revision
    /// was sealed.
    pub blake3_hex: String,
    /// `seq` of the most recent `undo_journal` row at snapshot time,
    /// or `None` if the snapshot was taken before any command had been
    /// applied. The replay engine uses this to know where in the
    /// command history the snapshot corresponds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_head_seq: Option<i64>,
    /// File size in bytes at snapshot time.
    pub size_bytes: u64,
}

impl Revision {
    /// Look up a tracked entity by `(category, id)`.
    pub fn find_entity(&self, category: &str, id: &str) -> Option<&RevisionEntity> {
        self.tracked_entities
            .iter()
            .find(|e| e.category == category && e.id == id)
    }

    /// Group tracked entities by category. Returns a stable
    /// `BTreeMap` so the order is deterministic for tests and UIs.
    pub fn entities_by_category(&self) -> BTreeMap<&str, Vec<&RevisionEntity>> {
        let mut out: BTreeMap<&str, Vec<&RevisionEntity>> = BTreeMap::new();
        for e in &self.tracked_entities {
            out.entry(e.category.as_str()).or_default().push(e);
        }
        out
    }
}

/// Builder used by callers (e.g. `aec_command`) that own the audit chain.
pub struct RevisionDraft {
    pub project_id: ProjectId,
    pub tag: String,
    pub description: String,
    pub audit_chain_head: String,
    pub manifest_name: String,
    pub manifest_app_version: String,
    pub tracked_entities: Vec<RevisionEntity>,
}

impl RevisionDraft {
    pub fn new(
        project_id: ProjectId,
        tag: impl Into<String>,
        description: impl Into<String>,
        audit_chain_head: impl Into<String>,
        manifest_name: impl Into<String>,
        manifest_app_version: impl Into<String>,
    ) -> Self {
        Self {
            project_id,
            tag: tag.into(),
            description: description.into(),
            audit_chain_head: audit_chain_head.into(),
            manifest_name: manifest_name.into(),
            manifest_app_version: manifest_app_version.into(),
            tracked_entities: Vec::new(),
        }
    }

    pub fn add_entity(mut self, entity: RevisionEntity) -> Self {
        self.tracked_entities.push(entity);
        self
    }

    fn finalize(self, id: String, created_at: DateTime<Utc>) -> Revision {
        Revision {
            id,
            project_id: self.project_id,
            tag: self.tag,
            description: self.description,
            created_at,
            audit_chain_head: self.audit_chain_head,
            manifest_name: self.manifest_name,
            manifest_app_version: self.manifest_app_version,
            tracked_entities: self.tracked_entities,
            snapshot: None,
        }
    }
}

/// On-disk store for revisions.
///
/// Backed by a `revisions/` directory inside the project package. Each
/// revision is a JSON file `<revision-id>.json`. The store does *not*
/// keep a sidecar index — it lists the directory on every read so the
/// filesystem is the source of truth.
#[derive(Debug, Clone)]
pub struct RevisionStore {
    dir: PathBuf,
}

impl RevisionStore {
    /// Open a store rooted at `<project>/revisions/`. Creates the
    /// directory if missing.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, AecError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Directory the store reads from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Persist a new revision. Generates an id and timestamp; rejects
    /// duplicate tag names within the same store.
    ///
    /// Does NOT capture an on-disk database snapshot. Use
    /// [`Self::create_with_snapshot`] for the deliverable-grade
    /// snapshot path that copies `project.sqlite` into the revisions
    /// directory and records its BLAKE3 hash.
    pub fn create(&self, draft: RevisionDraft) -> Result<Revision, AecError> {
        if draft.tag.trim().is_empty() {
            return Err(AecError::Other("revision tag must not be empty".into()));
        }
        for existing in self.list()? {
            if existing.tag == draft.tag {
                return Err(AecError::AlreadyExists(format!(
                    "revision tag already exists: {}",
                    draft.tag
                )));
            }
        }
        let id = format!("rev_{}", Uuid::new_v4().simple());
        let revision = draft.finalize(id, Utc::now());
        self.write_atomic(&revision)?;
        Ok(revision)
    }

    /// Persist a new revision **and** capture a real filesystem
    /// snapshot of the project database.
    ///
    /// The flow is:
    ///
    /// 1. Reject empty / duplicate tags exactly like [`Self::create`].
    /// 2. Run `PRAGMA wal_checkpoint(TRUNCATE)` against `live_conn`
    ///    so every committed transaction in the WAL is folded into
    ///    the main DB file, and inspect its `(busy, log, checkpointed)`
    ///    result row — abort with an error if `busy != 0` so a
    ///    partial checkpoint never produces an inconsistent snapshot.
    /// 3. Read `live_db_path` and write the bytes atomically to
    ///    `<revisions-dir>/<revision-id>.snap` (write-to-`.tmp`,
    ///    rename).
    /// 4. Compute the BLAKE3 of the snapshot bytes and the file size.
    /// 5. Look up the current `MAX(seq)` in `undo_journal` to record
    ///    the journal head pointer alongside the snapshot.
    /// 6. Finalise the [`Revision`] (with [`Self::write_atomic`]) and
    ///    return it.
    ///
    /// The caller is expected to hold a write lock for the *entire*
    /// call — not just between the checkpoint and the file copy. The
    /// journal-head query in step 5 reads `MAX(seq)` from the live
    /// connection, so any concurrent writer between steps 3 and 5
    /// would record a head pointer that's strictly ahead of what's in
    /// the snapshot.
    ///
    /// If any step after the snapshot file is written fails, the
    /// already-written `.snap` is removed so the revisions directory
    /// never carries a half-built snapshot without a sibling `.json`
    /// metadata file.
    pub fn create_with_snapshot(
        &self,
        draft: RevisionDraft,
        live_conn: &Connection,
        live_db_path: &Path,
    ) -> Result<Revision, AecError> {
        if draft.tag.trim().is_empty() {
            return Err(AecError::Other("revision tag must not be empty".into()));
        }
        for existing in self.list()? {
            if existing.tag == draft.tag {
                return Err(AecError::AlreadyExists(format!(
                    "revision tag already exists: {}",
                    draft.tag
                )));
            }
        }

        // 1) Force the WAL to fold into the main DB file so a byte-
        //    level copy is consistent. `TRUNCATE` is the strongest
        //    checkpoint mode SQLite offers — it both folds the WAL
        //    into the main DB file and truncates the WAL file to
        //    zero length.
        //
        //    Important: `PRAGMA wal_checkpoint` returns
        //    `(busy, log, checkpointed)` and SQLite can fall back to
        //    a *partial* checkpoint (e.g. PASSIVE semantics) if a
        //    concurrent reader holds a WAL snapshot past the
        //    `busy_timeout`. In that case the main DB file is still
        //    missing the most-recent committed frames, so the
        //    byte-level copy we are about to take would be an
        //    inconsistent snapshot. We must inspect the `busy` column
        //    and abort here rather than silently producing a
        //    truncated snapshot whose BLAKE3 still happens to match
        //    `verify_snapshot`.
        let busy: i32 = live_conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|e| AecError::Other(format!("WAL checkpoint failed: {e}")))?;
        if busy != 0 {
            return Err(AecError::Other(
                "WAL checkpoint could not complete: a concurrent reader held a WAL snapshot \
                 past the busy timeout, so the on-disk db file is missing recently committed \
                 frames and a byte-level snapshot would be inconsistent"
                    .into(),
            ));
        }

        let id = format!("rev_{}", Uuid::new_v4().simple());
        let snap_relative = format!("{id}.snap");
        let snap_path = self.dir.join(&snap_relative);

        // 2) Write the snapshot file atomically (tmp + rename).
        let tmp_path = snap_path.with_extension("snap.tmp");
        fs::copy(live_db_path, &tmp_path)
            .map_err(|e| AecError::Other(format!("snapshot copy failed: {e}")))?;
        if let Err(e) = fs::rename(&tmp_path, &snap_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(AecError::Other(format!("snapshot rename failed: {e}")));
        }

        // 3) Compute BLAKE3 + size from the file we just wrote.
        let (blake3_hex, size_bytes) = match blake3_of_file(&snap_path) {
            Ok(out) => out,
            Err(e) => {
                let _ = fs::remove_file(&snap_path);
                return Err(e);
            }
        };

        // 4) Read the journal head pointer from the live db. The
        //    `undo_journal` table is created by `aec_core::db` so it
        //    is guaranteed to exist; an empty journal returns `None`.
        let journal_head_seq = match journal_max_seq(live_conn) {
            Ok(head) => head,
            Err(e) => {
                let _ = fs::remove_file(&snap_path);
                return Err(e);
            }
        };

        let mut revision = draft.finalize(id, Utc::now());
        revision.snapshot = Some(RevisionSnapshot {
            relative_path: snap_relative,
            blake3_hex,
            journal_head_seq,
            size_bytes,
        });

        if let Err(e) = self.write_atomic(&revision) {
            let _ = fs::remove_file(&snap_path);
            return Err(e);
        }
        Ok(revision)
    }

    /// Absolute path of a revision's snapshot file inside this store.
    pub fn snapshot_path(&self, revision: &Revision) -> Option<PathBuf> {
        revision
            .snapshot
            .as_ref()
            .map(|s| self.dir.join(&s.relative_path))
    }

    /// Re-hash the on-disk snapshot file and compare it against the
    /// digest stored in the revision metadata. Returns `true` when the
    /// file is byte-identical to what was captured at snapshot time.
    /// Returns an error if the snapshot file is missing or unreadable.
    pub fn verify_snapshot(&self, revision: &Revision) -> Result<bool, AecError> {
        let Some(meta) = revision.snapshot.as_ref() else {
            return Ok(false);
        };
        let snap_path = self.dir.join(&meta.relative_path);
        let (hex, _size) = blake3_of_file(&snap_path)?;
        Ok(hex == meta.blake3_hex)
    }

    /// List all revisions in creation-time order (oldest first).
    pub fn list(&self) -> Result<Vec<Revision>, AecError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            let r: Revision = serde_json::from_slice(&bytes)
                .map_err(|e| AecError::Other(format!("revision parse error: {e}")))?;
            out.push(r);
        }
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    /// Look up a revision by id.
    pub fn get(&self, id: &str) -> Result<Option<Revision>, AecError> {
        let path = self.dir.join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let r: Revision = serde_json::from_slice(&bytes)
            .map_err(|e| AecError::Other(format!("revision parse error: {e}")))?;
        Ok(Some(r))
    }

    /// Look up a revision by user-facing tag.
    pub fn find_by_tag(&self, tag: &str) -> Result<Option<Revision>, AecError> {
        Ok(self.list()?.into_iter().find(|r| r.tag == tag))
    }

    /// Remove a revision file. Also removes the associated snapshot
    /// file (`<id>.snap`) if one exists, so a delete leaves no orphan
    /// snapshot bytes behind. Returns `true` if at least the metadata
    /// file existed (and was deleted).
    pub fn delete(&self, id: &str) -> Result<bool, AecError> {
        let json_path = self.dir.join(format!("{id}.json"));
        let snap_path = self.dir.join(format!("{id}.snap"));
        let existed = json_path.exists();
        if !existed {
            return Ok(false);
        }
        fs::remove_file(&json_path)?;
        if snap_path.exists() {
            fs::remove_file(&snap_path)?;
        }
        Ok(true)
    }

    fn write_atomic(&self, revision: &Revision) -> Result<(), AecError> {
        let bytes = serde_json::to_vec_pretty(revision)
            .map_err(|e| AecError::Other(format!("revision serialize error: {e}")))?;
        let final_path = self.dir.join(format!("{}.json", revision.id));
        let tmp_path = final_path.with_extension("json.tmp");
        fs::write(&tmp_path, bytes)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

/// Compute BLAKE3 of a file in streaming fashion (no full-file
/// read into memory), returning `(hex_digest, size_bytes)`.
fn blake3_of_file(path: &Path) -> Result<(String, u64), AecError> {
    let mut file = fs::File::open(path)
        .map_err(|e| AecError::Other(format!("open snapshot for hashing: {e}")))?;
    let mut hasher = blake3::Hasher::new();
    // 64 KiB read buffer. Allocated on the heap to keep the stack
    // small (BLAKE3 streaming is happy with any buffer size; this is
    // a throughput/locality trade-off, not a correctness constraint).
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AecError::Other(format!("read snapshot for hashing: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

/// Read `MAX(seq)` from the `undo_journal` table. Returns `None` for
/// an empty journal. Returns an error only when the SQL itself fails
/// — a missing table is treated as "no journal yet" and returns
/// `Ok(None)` because some early-stage tests skip schema bootstrap.
fn journal_max_seq(conn: &Connection) -> Result<Option<i64>, AecError> {
    let mut has_table = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='undo_journal'")
        .map_err(|e| AecError::Other(format!("probe undo_journal table: {e}")))?;
    let exists = has_table
        .exists([])
        .map_err(|e| AecError::Other(format!("probe undo_journal table: {e}")))?;
    if !exists {
        return Ok(None);
    }
    let mut stmt = conn
        .prepare("SELECT MAX(seq) FROM undo_journal")
        .map_err(|e| AecError::Other(format!("query undo_journal head: {e}")))?;
    let head: Option<i64> = stmt
        .query_row([], |r| r.get::<_, Option<i64>>(0))
        .map_err(|e| AecError::Other(format!("query undo_journal head: {e}")))?;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_entity(category: &str, id: &str, hash: &str) -> RevisionEntity {
        RevisionEntity {
            category: category.into(),
            id: id.into(),
            payload_hash: hash.into(),
            label: None,
        }
    }

    fn fixture_draft(project_id: &ProjectId, tag: &str) -> RevisionDraft {
        RevisionDraft::new(
            project_id.clone(),
            tag,
            "demo revision",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "Apartment 12B",
            "0.1.0",
        )
        .add_entity(fixture_entity("geometry", "wall.A1", "deadbeef00"))
        .add_entity(fixture_entity("sheet", "A101", "cafe00cafe"))
    }

    #[test]
    fn create_and_list_revisions() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();

        let v1 = store.create(fixture_draft(&pid, "v1")).unwrap();
        let v2 = store.create(fixture_draft(&pid, "v2")).unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);
        // Sorted oldest first.
        assert_eq!(listed[0].id, v1.id);
        assert_eq!(listed[1].id, v2.id);
        assert!(v1.id.starts_with("rev_"));
    }

    #[test]
    fn duplicate_tag_rejected() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();
        store.create(fixture_draft(&pid, "dup")).unwrap();
        let err = store.create(fixture_draft(&pid, "dup")).unwrap_err();
        assert!(matches!(err, AecError::AlreadyExists(_)));
    }

    #[test]
    fn empty_tag_rejected() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();
        let err = store.create(fixture_draft(&pid, "   ")).unwrap_err();
        assert!(matches!(err, AecError::Other(_)));
    }

    #[test]
    fn get_and_find_by_tag() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();
        let r = store.create(fixture_draft(&pid, "client-review")).unwrap();
        let by_id = store.get(&r.id).unwrap().unwrap();
        assert_eq!(by_id, r);
        let by_tag = store.find_by_tag("client-review").unwrap().unwrap();
        assert_eq!(by_tag.id, r.id);
        assert!(store.get("nope").unwrap().is_none());
        assert!(store.find_by_tag("nope").unwrap().is_none());
    }

    #[test]
    fn delete_removes_file() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();
        let r = store.create(fixture_draft(&pid, "to-delete")).unwrap();
        assert!(store.delete(&r.id).unwrap());
        assert!(store.get(&r.id).unwrap().is_none());
        assert!(!store.delete(&r.id).unwrap());
    }

    #[test]
    fn entities_by_category_groups_correctly() {
        let td = TempDir::new().unwrap();
        let store = RevisionStore::open(td.path()).unwrap();
        let pid = ProjectId::new();
        let mut draft = fixture_draft(&pid, "groups");
        draft
            .tracked_entities
            .push(fixture_entity("geometry", "wall.B1", "00aa"));
        let r = store.create(draft).unwrap();
        let grouped = r.entities_by_category();
        assert_eq!(grouped["geometry"].len(), 2);
        assert_eq!(grouped["sheet"].len(), 1);
    }

    #[test]
    fn store_survives_reopen() {
        let td = TempDir::new().unwrap();
        let pid = ProjectId::new();
        {
            let store = RevisionStore::open(td.path()).unwrap();
            store.create(fixture_draft(&pid, "persistence")).unwrap();
        }
        let store = RevisionStore::open(td.path()).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].tag, "persistence");
    }

    fn write_dummy_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE entities (id TEXT PRIMARY KEY, kind TEXT NOT NULL, body TEXT NOT NULL);
             CREATE TABLE undo_journal (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 command_id TEXT NOT NULL,
                 applied_at TEXT NOT NULL,
                 forward TEXT NOT NULL,
                 inverse TEXT NOT NULL,
                 superseded INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entities (id, kind, body) VALUES ('wall.A', 'wall', '{\"a\":1}')",
            [],
        )
        .unwrap();
        conn
    }

    fn append_journal(conn: &Connection, cmd_id: &str) {
        conn.execute(
            "INSERT INTO undo_journal (command_id, applied_at, forward, inverse)
             VALUES (?1, datetime('now'), '[]', '[]')",
            [cmd_id],
        )
        .unwrap();
    }

    #[test]
    fn create_with_snapshot_writes_real_db_file_with_blake3_match() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);
        append_journal(&conn, "cmd_1");
        append_journal(&conn, "cmd_2");

        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();
        let draft = fixture_draft(&ProjectId::new(), "v1");
        let rev = store.create_with_snapshot(draft, &conn, &db_path).unwrap();

        let snap = rev
            .snapshot
            .as_ref()
            .expect("snapshot metadata should be populated");
        assert!(snap.relative_path.starts_with(&rev.id));
        assert!(std::path::Path::new(&snap.relative_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("snap")));
        assert!(snap.size_bytes > 0);
        assert_eq!(snap.journal_head_seq, Some(2));
        assert_eq!(snap.blake3_hex.len(), 64);

        // The .snap file exists at the recorded path and the BLAKE3
        // recomputed from disk matches the stored hex digest.
        let snap_path = store.snapshot_path(&rev).unwrap();
        assert!(snap_path.exists());
        let (recomputed, _) = blake3_of_file(&snap_path).unwrap();
        assert_eq!(recomputed, snap.blake3_hex);
        assert!(store.verify_snapshot(&rev).unwrap());
    }

    #[test]
    fn snapshots_are_independent_of_subsequent_live_db_mutations() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);

        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();
        let r1 = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "v1"), &conn, &db_path)
            .unwrap();
        let blake3_v1 = r1.snapshot.as_ref().unwrap().blake3_hex.clone();

        // Mutate the live db after the snapshot is sealed.
        conn.execute(
            "INSERT INTO entities (id, kind, body) VALUES ('wall.B', 'wall', '{\"a\":2}')",
            [],
        )
        .unwrap();
        append_journal(&conn, "cmd_after_v1");

        let r2 = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "v2"), &conn, &db_path)
            .unwrap();
        let blake3_v2 = r2.snapshot.as_ref().unwrap().blake3_hex.clone();

        // The two snapshots have different content hashes and exist
        // as independent on-disk files.
        assert_ne!(blake3_v1, blake3_v2);
        assert!(store.snapshot_path(&r1).unwrap().exists());
        assert!(store.snapshot_path(&r2).unwrap().exists());

        // v1's hash still verifies — i.e. mutating the live db did NOT
        // alter the v1 .snap file on disk.
        assert!(store.verify_snapshot(&r1).unwrap());
        assert!(store.verify_snapshot(&r2).unwrap());

        // Journal head pointer advanced between snapshots.
        assert_eq!(r1.snapshot.as_ref().unwrap().journal_head_seq, None);
        assert_eq!(r2.snapshot.as_ref().unwrap().journal_head_seq, Some(1));
    }

    #[test]
    fn verify_snapshot_detects_tampering() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);

        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();
        let rev = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "tamper"), &conn, &db_path)
            .unwrap();

        // Tamper with the .snap file by appending a byte.
        let snap_path = store.snapshot_path(&rev).unwrap();
        let mut bytes = std::fs::read(&snap_path).unwrap();
        bytes.push(0xAA);
        std::fs::write(&snap_path, bytes).unwrap();

        assert!(
            !store.verify_snapshot(&rev).unwrap(),
            "verify_snapshot must reject a tampered .snap file"
        );
    }

    #[test]
    fn create_with_snapshot_rejects_duplicate_tag_and_empty_tag() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);
        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();

        store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "dup"), &conn, &db_path)
            .unwrap();
        let err = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "dup"), &conn, &db_path)
            .unwrap_err();
        assert!(matches!(err, AecError::AlreadyExists(_)));

        let err = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), ""), &conn, &db_path)
            .unwrap_err();
        assert!(matches!(err, AecError::Other(_)));
    }

    #[test]
    fn delete_removes_snapshot_file_alongside_metadata() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);
        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();
        let rev = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "del"), &conn, &db_path)
            .unwrap();
        let snap_path = store.snapshot_path(&rev).unwrap();
        assert!(snap_path.exists());

        assert!(store.delete(&rev.id).unwrap());
        assert!(!snap_path.exists());
        assert!(store.get(&rev.id).unwrap().is_none());
    }

    #[test]
    fn snapshot_serde_round_trip_preserves_metadata() {
        let proj = TempDir::new().unwrap();
        let db_path = proj.path().join("project.sqlite");
        let conn = write_dummy_db(&db_path);
        append_journal(&conn, "cmd_1");
        let store = RevisionStore::open(proj.path().join("revisions")).unwrap();
        let rev = store
            .create_with_snapshot(fixture_draft(&ProjectId::new(), "serde"), &conn, &db_path)
            .unwrap();

        // Round-trip through the JSON sidecar to make sure the
        // `snapshot` field made it onto disk + back.
        let reloaded = store.get(&rev.id).unwrap().unwrap();
        assert_eq!(reloaded.snapshot, rev.snapshot);
    }
}
