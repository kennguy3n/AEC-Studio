//! v2 → v3 migration: tag every `undo_journal` row with the originating
//! command's `Scope`.
//!
//! Background. `aec_command::CommandEngine` is opened with an
//! `active_scope` (Design / Draft / Bim / Render / Deliver). The
//! engine's `compute_deltas` already rejects forward commands whose
//! `CommandKind::scope()` doesn't match `active_scope`, so a
//! `Design`-scoped engine can never apply a `Draft`-only command.
//! That invariant only guards the *forward* path though: the undo
//! and redo paths (`CommandEngine::undo_persistent` /
//! `redo_persistent`) had no symmetric check because the journal
//! entry didn't carry the scope it was created under.
//!
//! The renderer-facing `BridgeBackend::commandUndo(activeScope, …)`
//! contract documents that the engine validates `activeScope`
//! matches the entry's scope. v3 makes that contract enforceable by
//! adding a `scope` column to `undo_journal` so every row records
//! the scope of the originating command.
//!
//! Migration safety:
//!
//! - Pre-existing rows are tagged `design` (the default scope for
//!   Phase 1/2 projects). Practically every command on a real
//!   project today is `Design`-scoped, so the default is correct
//!   for legacy data — and even if it weren't, the only consequence
//!   is that an undo on a non-Design scope would surface a clear
//!   error rather than silently undoing into the wrong scope.
//! - The column is `NOT NULL DEFAULT 'design'` so the existing
//!   reader/writer paths can keep functioning if a stale binary
//!   re-opens the database (it just sees the default).
//! - Forward-only: there is no downgrade. A v3 database opened by a
//!   binary that only knows v2 will fail
//!   `ProjectManifest::validate` with `SchemaMismatch`, matching the
//!   forward-version-rejection rule documented in
//!   [`crate::migrations`].

use crate::migrations::Migration;

pub const V3_UNDO_JOURNAL_SCOPE: Migration = Migration {
    target: 3,
    description: "tag undo_journal rows with the originating command's scope",
    up_sql: "
        ALTER TABLE undo_journal ADD COLUMN scope TEXT NOT NULL DEFAULT 'design';
        CREATE INDEX idx_undo_journal_scope ON undo_journal(scope);
    ",
};
