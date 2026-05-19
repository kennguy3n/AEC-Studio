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

    pub fn pop_undo(&mut self) -> Option<JournalEntry> {
        let entry = self.undo.pop()?;
        self.redo.push(entry.clone());
        Some(entry)
    }

    pub fn pop_redo(&mut self) -> Option<JournalEntry> {
        let entry = self.redo.pop()?;
        self.undo.push(entry.clone());
        Some(entry)
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
        let popped = j.pop_undo().unwrap();
        assert_eq!(j.undo_len(), 0);
        assert_eq!(j.redo_len(), 1);
        let redone = j.pop_redo().unwrap();
        assert_eq!(redone.command_id, popped.command_id);
        assert_eq!(j.undo_len(), 1);
    }

    #[test]
    fn new_record_clears_redo_stack() {
        let mut j = UndoRedoJournal::with_capacity(0);
        j.record(entry());
        j.pop_undo().unwrap();
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
