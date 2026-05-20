//! One-way sync from KChat threads to the project audit trail.
//!
//! Only ingests *into* the project — never exports project data
//! back to KChat. The sync is deduplicated by
//! `(thread_id, timestamp, commenter)` so re-importing the same batch
//! does not produce duplicate audit entries.
//!
//! `CommentSync` is **stateful**: it keeps a `HashSet` of dedup keys
//! it has already seen so a long-running session can call `ingest` /
//! `ingest_batch` repeatedly without manual bookkeeping. Persisting
//! the dedup state across app restarts is the caller's job — the
//! audit trail itself acts as the canonical persisted record, so a
//! `CommentSync` rehydrated from an audit log (via
//! [`CommentSync::from_existing_entries`]) recovers the dedup set
//! automatically.

use std::collections::HashSet;

use crate::kchat::{ingest_review, ProjectAuditEntry, ReviewComment};

/// Stateful comment-sync engine.
#[derive(Debug, Default)]
pub struct CommentSync {
    seen: HashSet<(String, chrono::DateTime<chrono::Utc>, String)>,
}

impl CommentSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild a sync's dedup state from previously persisted audit
    /// entries. Useful when an AEC Studio session resumes a project
    /// that already has KChat-sourced audit history.
    pub fn from_existing_entries<'a, I>(entries: I) -> Self
    where
        I: IntoIterator<Item = &'a ProjectAuditEntry>,
    {
        let seen = entries
            .into_iter()
            .filter(|e| e.actor.kind == crate::types::ActorKind::KChat)
            .map(|e| {
                (
                    e.thread_id.clone(),
                    e.timestamp,
                    e.actor.tool.clone().unwrap_or_default(),
                )
            })
            .collect();
        Self { seen }
    }

    /// Ingest a single comment. Returns `Some(entry)` if it was new,
    /// `None` if it was already known.
    pub fn ingest(&mut self, comment: ReviewComment) -> Option<ProjectAuditEntry> {
        let key = comment.dedup_key();
        if self.seen.contains(&key) {
            return None;
        }
        self.seen.insert(key);
        Some(ingest_review(comment))
    }

    /// Ingest a batch of comments. Returns the entries that were
    /// new (in input order, duplicates omitted).
    pub fn ingest_batch<I>(&mut self, comments: I) -> Vec<ProjectAuditEntry>
    where
        I: IntoIterator<Item = ReviewComment>,
    {
        let mut out = Vec::new();
        for c in comments {
            if let Some(entry) = self.ingest(c) {
                out.push(entry);
            }
        }
        out
    }

    /// Number of unique dedup keys seen so far.
    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kchat::{ingest_review, ApprovalStatus};

    fn make_comment(thread: &str, commenter: &str, text: &str, secs_offset: i64) -> ReviewComment {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000 + secs_offset, 0)
            .unwrap();
        ReviewComment {
            thread_id: thread.into(),
            commenter: commenter.into(),
            text: text.into(),
            timestamp: ts,
            artifact_ref: None,
        }
    }

    #[test]
    fn batch_sync_appends_n_new_comments() {
        let mut sync = CommentSync::new();
        let entries = sync.ingest_batch(vec![
            make_comment("t1", "@a", "first", 0),
            make_comment("t1", "@b", "second", 1),
            make_comment("t1", "@a", "third", 2),
        ]);
        assert_eq!(entries.len(), 3);
        assert_eq!(sync.seen_count(), 3);
    }

    #[test]
    fn dedup_ignores_reimported_comments() {
        let mut sync = CommentSync::new();
        let c = make_comment("t1", "@a", "hi", 0);
        let _first = sync.ingest(c.clone()).unwrap();
        let again = sync.ingest(c);
        assert!(again.is_none());
        assert_eq!(sync.seen_count(), 1);
    }

    #[test]
    fn dedup_persists_across_batches() {
        let mut sync = CommentSync::new();
        let initial = vec![
            make_comment("t1", "@a", "first", 0),
            make_comment("t1", "@b", "second", 1),
        ];
        sync.ingest_batch(initial.clone());
        let mut reimport = initial;
        reimport.push(make_comment("t1", "@c", "third", 2));
        let added = sync.ingest_batch(reimport);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].text, "third");
        assert_eq!(sync.seen_count(), 3);
    }

    #[test]
    fn from_existing_entries_seeds_dedup_state() {
        // Pretend the project already has two KChat-sourced audit
        // entries from a prior session. The new CommentSync should
        // pick up those keys and refuse to re-ingest them.
        let entry_a = ingest_review(make_comment("t1", "@a", "old", 0));
        let entry_b = ingest_review(make_comment("t1", "@b", "older", 1));
        let mut sync = CommentSync::from_existing_entries([&entry_a, &entry_b]);
        let reimport = sync.ingest_batch(vec![
            make_comment("t1", "@a", "old", 0),
            make_comment("t1", "@b", "older", 1),
            make_comment("t1", "@a", "fresh", 5),
        ]);
        assert_eq!(reimport.len(), 1);
        assert_eq!(reimport[0].text, "fresh");
    }

    #[test]
    fn different_threads_with_same_timestamp_are_distinct() {
        let mut sync = CommentSync::new();
        let added = sync.ingest_batch(vec![
            make_comment("t1", "@a", "in 1", 0),
            make_comment("t2", "@a", "in 2", 0),
        ]);
        assert_eq!(added.len(), 2);
    }

    #[test]
    fn approval_status_is_not_propagated_by_plain_comment_sync() {
        // Plain CommentSync only handles bare ReviewComments. Status
        // round-tripping is handled by `kchat::ingest_review_card`
        // and the integration layer; this test guards the explicit
        // contract that CommentSync entries never carry approval
        // status by accident.
        let mut sync = CommentSync::new();
        let entry = sync.ingest(make_comment("t1", "@a", "yo", 9)).unwrap();
        assert!(entry.status.is_none());
        // sanity: ApprovalStatus exists and is a public enum.
        let _ = ApprovalStatus::Commented;
    }
}
