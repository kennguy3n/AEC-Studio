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
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
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

    /// Remove a revision file. Used for tests and for explicit deletion
    /// by the UI.
    pub fn delete(&self, id: &str) -> Result<bool, AecError> {
        let path = self.dir.join(format!("{id}.json"));
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path)?;
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
}
