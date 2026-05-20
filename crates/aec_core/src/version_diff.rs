//! Diff two revisions at the tracked-entity level.
//!
//! Entities are matched by `(category, id)`. An entity that exists in
//! both revisions but whose `payload_hash` changed is reported as
//! "modified". Entities only in the older revision are "removed";
//! entities only in the newer one are "added". This gives the Deliver
//! mode UI everything it needs to show the change summary between two
//! tagged revisions without a full project re-load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::revision::{Revision, RevisionEntity};

/// Per-category counts for a [`VersionDiff`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffCounts {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub unchanged: usize,
}

impl DiffCounts {
    pub fn total_changes(&self) -> usize {
        self.added + self.removed + self.modified
    }
}

/// A single entity change reported by [`compare_revisions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityChange {
    pub category: String,
    pub id: String,
    pub kind: EntityChangeKind,
    /// Hash before the change. `None` for newly added entities.
    pub before_hash: Option<String>,
    /// Hash after the change. `None` for removed entities.
    pub after_hash: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityChangeKind {
    Added,
    Removed,
    Modified,
    Unchanged,
}

/// Diff between two revisions. Use [`compare_revisions`] to build one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionDiff {
    pub base_revision_id: String,
    pub head_revision_id: String,
    /// All entity-level changes. Sorted by `(category, id)` for stable
    /// rendering in tests and UIs.
    pub changes: Vec<EntityChange>,
    /// Per-category roll-up so the UI can render counts without
    /// re-scanning [`Self::changes`].
    pub by_category: BTreeMap<String, DiffCounts>,
}

impl VersionDiff {
    /// True when the two revisions reference the exact same entities
    /// with the exact same hashes — including no additions or removals.
    pub fn is_clean(&self) -> bool {
        self.by_category.values().all(|c| c.total_changes() == 0)
    }

    /// Total number of changed entities across every category.
    pub fn total_changes(&self) -> usize {
        self.by_category.values().map(|c| c.total_changes()).sum()
    }

    /// Returns the changes for one category, filtered by change kind.
    pub fn changes_in(&self, category: &str, kind: EntityChangeKind) -> Vec<&EntityChange> {
        self.changes
            .iter()
            .filter(|c| c.category == category && c.kind == kind)
            .collect()
    }
}

/// Diff two revisions. The first argument is the "older" revision
/// (base) and the second is the "newer" one (head). Order matters: an
/// entity present only in `head` is reported as Added; only in `base`
/// is Removed.
pub fn compare_revisions(base: &Revision, head: &Revision) -> VersionDiff {
    // Group both sides by (category, id) for O(1) lookup.
    type Key = (String, String);
    let mut base_map: BTreeMap<Key, &RevisionEntity> = BTreeMap::new();
    let mut head_map: BTreeMap<Key, &RevisionEntity> = BTreeMap::new();
    for e in &base.tracked_entities {
        base_map.insert((e.category.clone(), e.id.clone()), e);
    }
    for e in &head.tracked_entities {
        head_map.insert((e.category.clone(), e.id.clone()), e);
    }

    let mut changes: Vec<EntityChange> = Vec::new();
    let mut by_category: BTreeMap<String, DiffCounts> = BTreeMap::new();

    // Walk the union of keys; BTreeMap iteration order is sorted, which
    // gives us a stable diff output for free.
    let all_keys: std::collections::BTreeSet<&Key> =
        base_map.keys().chain(head_map.keys()).collect();
    for key in all_keys {
        let (category, id) = key;
        let counts = by_category.entry(category.clone()).or_default();
        match (base_map.get(key), head_map.get(key)) {
            (Some(b), Some(h)) => {
                if b.payload_hash == h.payload_hash {
                    counts.unchanged += 1;
                    changes.push(EntityChange {
                        category: category.clone(),
                        id: id.clone(),
                        kind: EntityChangeKind::Unchanged,
                        before_hash: Some(b.payload_hash.clone()),
                        after_hash: Some(h.payload_hash.clone()),
                        label: h.label.clone().or_else(|| b.label.clone()),
                    });
                } else {
                    counts.modified += 1;
                    changes.push(EntityChange {
                        category: category.clone(),
                        id: id.clone(),
                        kind: EntityChangeKind::Modified,
                        before_hash: Some(b.payload_hash.clone()),
                        after_hash: Some(h.payload_hash.clone()),
                        label: h.label.clone().or_else(|| b.label.clone()),
                    });
                }
            }
            (None, Some(h)) => {
                counts.added += 1;
                changes.push(EntityChange {
                    category: category.clone(),
                    id: id.clone(),
                    kind: EntityChangeKind::Added,
                    before_hash: None,
                    after_hash: Some(h.payload_hash.clone()),
                    label: h.label.clone(),
                });
            }
            (Some(b), None) => {
                counts.removed += 1;
                changes.push(EntityChange {
                    category: category.clone(),
                    id: id.clone(),
                    kind: EntityChangeKind::Removed,
                    before_hash: Some(b.payload_hash.clone()),
                    after_hash: None,
                    label: b.label.clone(),
                });
            }
            (None, None) => unreachable!(),
        }
    }

    VersionDiff {
        base_revision_id: base.id.clone(),
        head_revision_id: head.id.clone(),
        changes,
        by_category,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision::RevisionDraft;
    use crate::types::ProjectId;
    use chrono::Utc;

    fn entity(category: &str, id: &str, hash: &str) -> RevisionEntity {
        RevisionEntity {
            category: category.into(),
            id: id.into(),
            payload_hash: hash.into(),
            label: None,
        }
    }

    fn revision(tag: &str, entities: Vec<RevisionEntity>) -> Revision {
        let draft = RevisionDraft {
            project_id: ProjectId::new(),
            tag: tag.into(),
            description: String::new(),
            audit_chain_head: String::new(),
            manifest_name: String::new(),
            manifest_app_version: String::new(),
            tracked_entities: entities,
        };
        // Manually finalize (we don't go through RevisionStore here
        // because we don't need to write to disk).
        Revision {
            id: format!("rev_{tag}"),
            project_id: draft.project_id,
            tag: draft.tag,
            description: draft.description,
            created_at: Utc::now(),
            audit_chain_head: draft.audit_chain_head,
            manifest_name: draft.manifest_name,
            manifest_app_version: draft.manifest_app_version,
            tracked_entities: draft.tracked_entities,
        }
    }

    #[test]
    fn detects_added_removed_modified_unchanged() {
        let base = revision(
            "v1",
            vec![
                entity("geometry", "wall.A1", "AA"),
                entity("geometry", "wall.A2", "BB"),
                entity("sheet", "A101", "CC"),
            ],
        );
        let head = revision(
            "v2",
            vec![
                // A1: unchanged
                entity("geometry", "wall.A1", "AA"),
                // A2: modified
                entity("geometry", "wall.A2", "BB-edited"),
                // sheet A101: removed (absent)
                // sheet A102: added
                entity("sheet", "A102", "DD"),
            ],
        );

        let diff = compare_revisions(&base, &head);
        assert_eq!(diff.total_changes(), 3);
        assert!(!diff.is_clean());

        let geom = &diff.by_category["geometry"];
        assert_eq!(geom.unchanged, 1);
        assert_eq!(geom.modified, 1);
        assert_eq!(geom.added, 0);
        assert_eq!(geom.removed, 0);

        let sheet = &diff.by_category["sheet"];
        assert_eq!(sheet.added, 1);
        assert_eq!(sheet.removed, 1);
        assert_eq!(sheet.modified, 0);
        assert_eq!(sheet.unchanged, 0);
    }

    #[test]
    fn identical_revisions_are_clean() {
        let entities = vec![entity("geometry", "wall.A1", "AA")];
        let base = revision("v1", entities.clone());
        let head = revision("v2", entities);
        let diff = compare_revisions(&base, &head);
        assert!(diff.is_clean());
        assert_eq!(diff.total_changes(), 0);
        assert_eq!(
            diff.changes
                .iter()
                .filter(|c| c.kind != EntityChangeKind::Unchanged)
                .count(),
            0
        );
    }

    #[test]
    fn changes_in_filters_by_kind() {
        let base = revision("v1", vec![entity("geometry", "wall.A1", "AA")]);
        let head = revision(
            "v2",
            vec![
                entity("geometry", "wall.A1", "AA-edited"),
                entity("geometry", "wall.A2", "BB"),
            ],
        );
        let diff = compare_revisions(&base, &head);
        let added = diff.changes_in("geometry", EntityChangeKind::Added);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].id, "wall.A2");
        let modified = diff.changes_in("geometry", EntityChangeKind::Modified);
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].id, "wall.A1");
    }

    #[test]
    fn diff_roundtrips_through_json() {
        let base = revision("v1", vec![entity("geometry", "wall.A1", "AA")]);
        let head = revision("v2", vec![entity("geometry", "wall.A1", "BB")]);
        let diff = compare_revisions(&base, &head);
        let j = serde_json::to_string(&diff).unwrap();
        let back: VersionDiff = serde_json::from_str(&j).unwrap();
        assert_eq!(diff, back);
    }
}
