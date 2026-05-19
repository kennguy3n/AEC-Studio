//! Undo/redo journal. The journal stores reversible deltas rather than
//! snapshots — replaying `inverse` undoes the command; replaying `forward`
//! redoes it.

use serde::{Deserialize, Serialize};

use aec_core::types::CommandId;

use crate::commands::EntityDelta;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub command_id: CommandId,
    pub applied_at: chrono::DateTime<chrono::Utc>,
    pub forward: Vec<EntityDelta>,
    pub inverse: Vec<EntityDelta>,
}

/// Bounded LIFO journal. The redo stack is cleared on a new forward command,
/// matching standard editor undo semantics.
#[derive(Debug, Default, Clone)]
pub struct UndoRedoJournal {
    undo: Vec<JournalEntry>,
    redo: Vec<JournalEntry>,
    capacity: usize,
}

impl UndoRedoJournal {
    /// `capacity == 0` means unbounded (Phase 2 keeps it bounded at 1024).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            capacity,
        }
    }

    pub fn record(&mut self, entry: JournalEntry) {
        self.undo.push(entry);
        self.redo.clear();
        if self.capacity > 0 && self.undo.len() > self.capacity {
            self.undo.remove(0);
        }
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Remove the top entry from the undo stack without touching redo.
    ///
    /// This is the **first** half of a two-phase undo. The caller must
    /// either complete the move with [`Self::commit_undone`] (after
    /// successfully replaying the inverse deltas) or restore the entry
    /// with [`Self::restore_undo`] (after a replay failure) so the
    /// journal never drifts out of sync with the graph.
    pub fn take_undo(&mut self) -> Option<JournalEntry> {
        self.undo.pop()
    }

    /// Push an entry back onto the undo stack — used after
    /// [`Self::take_undo`] when the replay failed and the graph was
    /// rolled back. The entry returns to the exact position it occupied
    /// before [`Self::take_undo`].
    pub fn restore_undo(&mut self, entry: JournalEntry) {
        self.undo.push(entry);
    }

    /// Move an entry that was taken from undo onto the redo stack. This
    /// is the **second** half of a two-phase undo and must only be
    /// called after the inverse deltas have been successfully applied.
    pub fn commit_undone(&mut self, entry: JournalEntry) {
        self.redo.push(entry);
    }

    /// Symmetric counterpart to [`Self::take_undo`] for redo.
    pub fn take_redo(&mut self) -> Option<JournalEntry> {
        self.redo.pop()
    }

    /// Symmetric counterpart to [`Self::restore_undo`] for redo.
    pub fn restore_redo(&mut self, entry: JournalEntry) {
        self.redo.push(entry);
    }

    /// Symmetric counterpart to [`Self::commit_undone`] for redo.
    pub fn commit_redone(&mut self, entry: JournalEntry) {
        self.undo.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{EntityDelta, EntityRecord};
    use aec_core::types::EntityId;

    fn entry() -> JournalEntry {
        let id = EntityId::new();
        let record = EntityRecord {
            id: id.clone(),
            kind: "wall".into(),
            body: serde_json::json!({}),
            parent: None,
        };
        JournalEntry {
            command_id: CommandId::new(),
            applied_at: chrono::Utc::now(),
            forward: vec![EntityDelta::Create {
                record: record.clone(),
            }],
            inverse: vec![EntityDelta::Delete { record }],
        }
    }

    #[test]
    fn record_then_undo_moves_to_redo() {
        let mut j = UndoRedoJournal::with_capacity(0);
        j.record(entry());
        assert_eq!(j.undo_len(), 1);
        let taken = j.take_undo().unwrap();
        // Take alone leaves the entry detached: not on either stack.
        assert_eq!(j.undo_len(), 0);
        assert_eq!(j.redo_len(), 0);
        j.commit_undone(taken.clone());
        assert_eq!(j.redo_len(), 1);
        let redo_taken = j.take_redo().unwrap();
        j.commit_redone(redo_taken.clone());
        assert_eq!(redo_taken.command_id, taken.command_id);
        assert_eq!(j.undo_len(), 1);
    }

    #[test]
    fn take_then_restore_undo_is_idempotent() {
        let mut j = UndoRedoJournal::with_capacity(0);
        let e = entry();
        j.record(e.clone());
        let taken = j.take_undo().unwrap();
        j.restore_undo(taken);
        assert_eq!(j.undo_len(), 1);
        assert_eq!(j.redo_len(), 0);
        // The restored entry is the same one we recorded.
        let head = j.take_undo().unwrap();
        assert_eq!(head.command_id, e.command_id);
    }

    #[test]
    fn new_record_clears_redo_stack() {
        let mut j = UndoRedoJournal::with_capacity(0);
        j.record(entry());
        let taken = j.take_undo().unwrap();
        j.commit_undone(taken);
        assert_eq!(j.redo_len(), 1);
        j.record(entry());
        assert_eq!(j.redo_len(), 0);
    }

    #[test]
    fn capacity_drops_oldest_entries() {
        let mut j = UndoRedoJournal::with_capacity(2);
        j.record(entry());
        j.record(entry());
        j.record(entry());
        assert_eq!(j.undo_len(), 2);
    }
}
