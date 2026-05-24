//! Undo/redo journal. The journal stores reversible deltas rather than
//! snapshots — replaying `inverse` undoes the command; replaying `forward`
//! redoes it.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};

use aec_core::types::{CommandId, Scope};

use crate::commands::EntityDelta;
use crate::error::{CommandError, CommandResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub command_id: CommandId,
    pub applied_at: chrono::DateTime<chrono::Utc>,
    /// Scope the originating command was executed under. The engine
    /// validates this on `undo` / `redo`: an entry recorded under
    /// `Scope::Bim` may not be undone while the active scope is
    /// `Scope::Design`. Without this field the only enforcement was on
    /// the *forward* path (in `compute_deltas`); the undo/redo paths
    /// would happily undo into the wrong scope.
    pub scope: Scope,
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

    /// Read-only peek at the top of the undo stack. Used by the
    /// persistent path to validate the next undo against the current
    /// graph state **before** taking the entry off the stack, so a
    /// validation failure leaves the journal completely unchanged.
    pub fn peek_undo(&self) -> Option<&JournalEntry> {
        self.undo.last()
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

    /// Symmetric counterpart to [`Self::peek_undo`] for redo.
    pub fn peek_redo(&self) -> Option<&JournalEntry> {
        self.redo.last()
    }

    /// Symmetric counterpart to [`Self::restore_undo`] for redo.
    pub fn restore_redo(&mut self, entry: JournalEntry) {
        self.redo.push(entry);
    }

    /// Symmetric counterpart to [`Self::commit_undone`] for redo.
    pub fn commit_redone(&mut self, entry: JournalEntry) {
        self.undo.push(entry);
    }

    /// Load the journal from the `undo_journal` table. Rows with
    /// `superseded = 0` are placed on the undo stack in seq order; the
    /// redo stack is loaded from `superseded = 1` rows (newest first).
    ///
    /// The redo stack must remain coherent across save/reload — a redo
    /// pushed *after* a take_undo+commit_undone keeps the entry's
    /// `superseded = 1` marker; a subsequent `record()` clears the redo
    /// stack in memory and on disk (see [`Self::persist_record`]).
    pub fn load(conn: &Connection, capacity: usize) -> CommandResult<Self> {
        let mut undo = Vec::new();
        let mut redo = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT seq, command_id, applied_at, forward, inverse, superseded, scope \
             FROM undo_journal ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![], |row| {
            let _seq: i64 = row.get(0)?;
            let command_id_str: String = row.get(1)?;
            let applied_at_str: String = row.get(2)?;
            let forward_str: String = row.get(3)?;
            let inverse_str: String = row.get(4)?;
            let superseded: i64 = row.get(5)?;
            let scope_str: String = row.get(6)?;
            Ok((
                command_id_str,
                applied_at_str,
                forward_str,
                inverse_str,
                superseded,
                scope_str,
            ))
        })?;
        for r in rows {
            let (command_id_str, applied_at_str, forward_str, inverse_str, superseded, scope_str) =
                r?;
            let command_id = CommandId::from_string(command_id_str)
                .map_err(|e| CommandError::JournalCorrupt(format!("invalid command id: {e}")))?;
            let applied_at: DateTime<Utc> = applied_at_str
                .parse::<DateTime<Utc>>()
                .map_err(|e| CommandError::JournalCorrupt(format!("invalid applied_at: {e}")))?;
            let scope = Scope::parse(&scope_str)
                .map_err(|e| CommandError::JournalCorrupt(format!("invalid scope: {e}")))?;
            let forward: Vec<EntityDelta> = serde_json::from_str(&forward_str)?;
            let inverse: Vec<EntityDelta> = serde_json::from_str(&inverse_str)?;
            let entry = JournalEntry {
                command_id,
                applied_at,
                scope,
                forward,
                inverse,
            };
            if superseded == 0 {
                undo.push(entry);
            } else {
                redo.push(entry);
            }
        }
        Ok(Self {
            undo,
            redo,
            capacity,
        })
    }

    /// Persist a freshly-recorded entry to the `undo_journal` table and
    /// mark any existing redo rows as removed (mirrors the in-memory
    /// `record()` behaviour of clearing the redo stack on a new forward
    /// command).
    pub fn persist_record(conn: &mut Connection, entry: &JournalEntry) -> CommandResult<()> {
        let tx = conn.transaction()?;
        Self::persist_record_in_tx(&tx, entry)?;
        tx.commit()?;
        Ok(())
    }

    /// SQL-only variant of [`Self::persist_record`] that operates inside
    /// an externally-managed transaction so the engine can combine the
    /// entity-delta writes and the journal record into a single atomic
    /// commit. Callers are responsible for calling `tx.commit()` (or
    /// dropping `tx` to roll back) themselves.
    pub fn persist_record_in_tx(tx: &Transaction, entry: &JournalEntry) -> CommandResult<()> {
        tx.execute("DELETE FROM undo_journal WHERE superseded = 1", params![])?;
        let forward = serde_json::to_string(&entry.forward)?;
        let inverse = serde_json::to_string(&entry.inverse)?;
        tx.execute(
            "INSERT INTO undo_journal (command_id, applied_at, forward, inverse, superseded, scope) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![
                entry.command_id.to_string(),
                entry.applied_at.to_rfc3339(),
                forward,
                inverse,
                entry.scope.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Move the top undo row to the redo stack (`superseded = 1`).
    /// Called after a successful `undo`.
    pub fn persist_undo(conn: &mut Connection, command_id: &CommandId) -> CommandResult<()> {
        let tx = conn.transaction()?;
        Self::persist_undo_in_tx(&tx, command_id)?;
        tx.commit()?;
        Ok(())
    }

    /// SQL-only variant of [`Self::persist_undo`]. See
    /// [`Self::persist_record_in_tx`] for the rationale.
    pub fn persist_undo_in_tx(tx: &Transaction, command_id: &CommandId) -> CommandResult<()> {
        let n = tx.execute(
            "UPDATE undo_journal SET superseded = 1 \
             WHERE seq = (SELECT MAX(seq) FROM undo_journal WHERE command_id = ?1 AND superseded = 0)",
            params![command_id.to_string()],
        )?;
        if n == 0 {
            return Err(CommandError::JournalCorrupt(format!(
                "undo persist: no row for command_id={command_id}"
            )));
        }
        Ok(())
    }

    /// Move the top redo row back to the undo stack (`superseded = 0`).
    /// Called after a successful `redo`.
    pub fn persist_redo(conn: &mut Connection, command_id: &CommandId) -> CommandResult<()> {
        let tx = conn.transaction()?;
        Self::persist_redo_in_tx(&tx, command_id)?;
        tx.commit()?;
        Ok(())
    }

    /// SQL-only variant of [`Self::persist_redo`]. See
    /// [`Self::persist_record_in_tx`] for the rationale.
    pub fn persist_redo_in_tx(tx: &Transaction, command_id: &CommandId) -> CommandResult<()> {
        let n = tx.execute(
            "UPDATE undo_journal SET superseded = 0 \
             WHERE seq = (SELECT MAX(seq) FROM undo_journal WHERE command_id = ?1 AND superseded = 1)",
            params![command_id.to_string()],
        )?;
        if n == 0 {
            return Err(CommandError::JournalCorrupt(format!(
                "redo persist: no row for command_id={command_id}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{EntityDelta, EntityRecord};
    use aec_core::types::EntityId;

    fn entry() -> JournalEntry {
        entry_with_scope(Scope::Design)
    }

    fn entry_with_scope(scope: Scope) -> JournalEntry {
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
            scope,
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

    fn open_in_memory_with_journal_table() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE undo_journal (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                command_id  TEXT NOT NULL,
                applied_at  TEXT NOT NULL,
                forward     TEXT NOT NULL,
                inverse     TEXT NOT NULL,
                superseded  INTEGER NOT NULL DEFAULT 0,
                scope       TEXT NOT NULL DEFAULT 'design'
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn persist_record_round_trips_scope() {
        let mut conn = open_in_memory_with_journal_table();
        let mut e = entry();
        e.scope = Scope::Bim;
        UndoRedoJournal::persist_record(&mut conn, &e).unwrap();
        let j = UndoRedoJournal::load(&conn, 1024).unwrap();
        assert_eq!(j.undo_len(), 1);
        let head = j.peek_undo().expect("undo stack has one entry");
        assert_eq!(head.scope, Scope::Bim);
    }

    #[test]
    fn persist_record_then_load_restores_undo_stack() {
        let mut conn = open_in_memory_with_journal_table();
        let e1 = entry();
        let e2 = entry();
        UndoRedoJournal::persist_record(&mut conn, &e1).unwrap();
        UndoRedoJournal::persist_record(&mut conn, &e2).unwrap();
        let j = UndoRedoJournal::load(&conn, 1024).unwrap();
        assert_eq!(j.undo_len(), 2);
        assert_eq!(j.redo_len(), 0);
    }

    #[test]
    fn persist_undo_marks_row_superseded_and_load_places_on_redo() {
        let mut conn = open_in_memory_with_journal_table();
        let e = entry();
        UndoRedoJournal::persist_record(&mut conn, &e).unwrap();
        UndoRedoJournal::persist_undo(&mut conn, &e.command_id).unwrap();
        let j = UndoRedoJournal::load(&conn, 1024).unwrap();
        assert_eq!(j.undo_len(), 0);
        assert_eq!(j.redo_len(), 1);
    }

    #[test]
    fn persist_redo_clears_superseded_and_load_places_on_undo() {
        let mut conn = open_in_memory_with_journal_table();
        let e = entry();
        UndoRedoJournal::persist_record(&mut conn, &e).unwrap();
        UndoRedoJournal::persist_undo(&mut conn, &e.command_id).unwrap();
        UndoRedoJournal::persist_redo(&mut conn, &e.command_id).unwrap();
        let j = UndoRedoJournal::load(&conn, 1024).unwrap();
        assert_eq!(j.undo_len(), 1);
        assert_eq!(j.redo_len(), 0);
    }

    #[test]
    fn persist_record_clears_redo_rows_on_disk() {
        let mut conn = open_in_memory_with_journal_table();
        // Build up an undo+redo state.
        let e1 = entry();
        UndoRedoJournal::persist_record(&mut conn, &e1).unwrap();
        UndoRedoJournal::persist_undo(&mut conn, &e1.command_id).unwrap();
        // e1 is now on the redo side. A new record should clear it.
        let e2 = entry();
        UndoRedoJournal::persist_record(&mut conn, &e2).unwrap();
        let j = UndoRedoJournal::load(&conn, 1024).unwrap();
        assert_eq!(j.undo_len(), 1);
        assert_eq!(j.redo_len(), 0);
    }
}
