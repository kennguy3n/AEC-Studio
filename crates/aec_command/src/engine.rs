//! The command engine that executes typed commands and journals them.
//!
//! The engine is **scope-aware**: it tracks the active workflow surface
//! (Design / Draft / Bim / Render / Deliver) and rejects commands that do
//! not match the active scope. AI plans flow through the same engine — the
//! AI sidecar emits a [`Command`] whose `actor.kind == Ai` and the
//! safety validator (see `aec_ai`) decides whether it gets executed.

use serde::{Deserialize, Serialize};

use aec_core::types::{CommandId, Scope};

use crate::audit::{AuditEnvelope, AuditHashChain};
use crate::commands::{Command, CommandKind, EntityDelta, ProjectGraph};
use crate::error::{CommandError, CommandResult as Result};
use crate::journal::{JournalEntry, UndoRedoJournal};

/// Execution mode: `Apply` commits to the graph, `DryRun` returns the diff
/// without mutating the graph (used by the AI panel's preview).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Apply,
    DryRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: CommandId,
    pub applied: Vec<EntityDelta>,
    pub audit: AuditEnvelope,
}

pub struct CommandEngine {
    graph: ProjectGraph,
    journal: UndoRedoJournal,
    audit: AuditHashChain,
    active_scope: Scope,
}

impl CommandEngine {
    pub fn new(active_scope: Scope) -> Self {
        Self {
            graph: ProjectGraph::new(),
            journal: UndoRedoJournal::with_capacity(1024),
            audit: AuditHashChain::new(),
            active_scope,
        }
    }

    pub fn graph(&self) -> &ProjectGraph {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut ProjectGraph {
        &mut self.graph
    }

    pub fn active_scope(&self) -> Scope {
        self.active_scope
    }

    pub fn set_active_scope(&mut self, scope: Scope) {
        self.active_scope = scope;
    }

    pub fn undo_len(&self) -> usize {
        self.journal.undo_len()
    }

    pub fn redo_len(&self) -> usize {
        self.journal.redo_len()
    }

    /// Execute a command, journaling the inverse deltas for undo.
    pub fn execute(&mut self, cmd: Command) -> Result<CommandResult> {
        let deltas = self.compute_deltas(&cmd.kind, &self.graph)?;
        let applied = self.apply_deltas(&deltas)?;
        let inverse: Vec<EntityDelta> = applied.iter().rev().map(EntityDelta::invert).collect();
        let envelope = self.audit.extend(
            &cmd.command_id,
            &serde_json::to_value(&cmd).unwrap_or(serde_json::Value::Null),
        );
        self.journal.record(JournalEntry {
            command_id: cmd.command_id.clone(),
            applied_at: cmd.ts,
            // Tag the entry with the active scope so the undo/redo
            // path can validate scope on pop. See `compute_deltas` for
            // the forward-side check.
            scope: self.active_scope,
            forward: applied.clone(),
            inverse,
        });
        Ok(CommandResult {
            command_id: cmd.command_id,
            applied,
            audit: envelope,
        })
    }

    /// Return the deltas this command *would* produce without mutating.
    pub fn dry_run(&self, kind: &CommandKind) -> Result<Vec<EntityDelta>> {
        self.compute_deltas(kind, &self.graph)
    }

    /// Undo the most recently executed command.
    ///
    /// The journal mutation is **two-phase**: we first detach the entry
    /// from the undo stack, then attempt to apply its inverse deltas.
    /// Only if the deltas apply cleanly do we commit the entry to the
    /// redo stack. If the apply fails (and the graph rolls itself back
    /// inside [`Self::apply_deltas`]) we put the entry back on the
    /// undo stack so the journal and the graph stay in lock-step.
    /// Without this, an `apply_deltas` failure would silently strand the
    /// entry on the redo stack and the next `redo` call would re-apply
    /// changes that are already in effect.
    pub fn undo(&mut self) -> Result<CommandResult> {
        // Validate scope before detaching the entry: if the active
        // scope disagrees with the recorded scope, the undo is rejected
        // and the journal is left untouched. Without this, an `undo`
        // issued under the wrong active scope would silently apply
        // inverse deltas tagged for a different rail.
        if let Some(top) = self.journal.peek_undo() {
            if top.scope != self.active_scope {
                return Err(CommandError::ScopeMismatch {
                    expected: top.scope.to_string(),
                    actual: self.active_scope.to_string(),
                });
            }
        }
        let entry = self
            .journal
            .take_undo()
            .ok_or(CommandError::NothingToUndo)?;
        let applied = match self.apply_deltas(&entry.inverse) {
            Ok(a) => a,
            Err(err) => {
                // Replay failed; graph is already rolled back inside
                // apply_deltas. Put the entry back where it came from so
                // a subsequent undo() retries the same operation rather
                // than skipping ahead.
                self.journal.restore_undo(entry);
                return Err(err);
            }
        };
        let envelope = self.audit.extend(
            &entry.command_id,
            &serde_json::json!({"undo": entry.command_id.as_str()}),
        );
        let result = CommandResult {
            command_id: entry.command_id.clone(),
            applied,
            audit: envelope,
        };
        self.journal.commit_undone(entry);
        Ok(result)
    }

    /// Redo the most recently undone command.
    ///
    /// Mirrors [`Self::undo`] with two-phase journal mutation — see that
    /// method's docs for the rationale.
    pub fn redo(&mut self) -> Result<CommandResult> {
        // Mirror of the scope-validation in [`Self::undo`].
        if let Some(top) = self.journal.peek_redo() {
            if top.scope != self.active_scope {
                return Err(CommandError::ScopeMismatch {
                    expected: top.scope.to_string(),
                    actual: self.active_scope.to_string(),
                });
            }
        }
        let entry = self
            .journal
            .take_redo()
            .ok_or(CommandError::NothingToRedo)?;
        let applied = match self.apply_deltas(&entry.forward) {
            Ok(a) => a,
            Err(err) => {
                self.journal.restore_redo(entry);
                return Err(err);
            }
        };
        let envelope = self.audit.extend(
            &entry.command_id,
            &serde_json::json!({"redo": entry.command_id.as_str()}),
        );
        let result = CommandResult {
            command_id: entry.command_id.clone(),
            applied,
            audit: envelope,
        };
        self.journal.commit_redone(entry);
        Ok(result)
    }

    /// Apply a sequence of deltas atomically. If one fails, deltas already
    /// applied are reverted so the graph stays consistent.
    fn apply_deltas(&mut self, deltas: &[EntityDelta]) -> Result<Vec<EntityDelta>> {
        let mut applied: Vec<EntityDelta> = Vec::with_capacity(deltas.len());
        for d in deltas {
            if let Err(err) = self.graph.apply(d) {
                // Roll back already-applied deltas in reverse.
                for done in applied.iter().rev() {
                    let _ = self.graph.apply(&done.invert());
                }
                return Err(err);
            }
            applied.push(d.clone());
        }
        Ok(applied)
    }

    /// Compute the forward deltas a [`CommandKind`] would produce against
    /// a caller-supplied graph view.
    ///
    /// Single-command callers ([`Self::execute`], [`Self::execute_persistent`],
    /// [`Self::dry_run`]) pass `&self.graph`. The batch path
    /// ([`Self::execute_persistent_batch`]) passes a forward-running
    /// shadow clone so command `i` sees the post-state of commands
    /// `0..i` — i.e. cross-command dependencies (e.g. one DXF command
    /// creating a layer and a later one referencing that layer) resolve
    /// correctly inside a batch.
    ///
    /// The scope-mismatch check stays on `self` because scope is engine
    /// state, not graph state.
    fn compute_deltas(&self, kind: &CommandKind, graph: &ProjectGraph) -> Result<Vec<EntityDelta>> {
        if kind.scope() != self.active_scope {
            return Err(CommandError::ScopeMismatch {
                expected: kind.scope().to_string(),
                actual: self.active_scope.to_string(),
            });
        }
        Self::compute_deltas_against_graph(graph, kind)
    }

    /// Pure delta computation against an externally-supplied graph.
    /// Used by [`Self::compute_deltas`] (with `&self.graph`) AND by
    /// [`Self::execute_persistent_batch`] (with a forward-walking
    /// staging clone) so each command in a batch validates against
    /// the post-state of the previous commands.
    ///
    /// Does NOT enforce scope \u2014 the caller is expected to check
    /// scope once for the whole batch.
    fn compute_deltas_against_graph(
        graph: &ProjectGraph,
        kind: &CommandKind,
    ) -> Result<Vec<EntityDelta>> {
        Ok(match kind {
            CommandKind::CreateWall(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::MoveWall(c) => vec![c.to_delta(graph)?],
            CommandKind::DeleteWall(c) => vec![c.to_delta(graph)?],
            CommandKind::CreateRoom(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::ModifyRoom(c) => vec![c.to_delta(graph)?],
            CommandKind::CreateFloor(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::ModifyFloor(c) => vec![c.to_delta(graph)?],
            CommandKind::CreateCeiling(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::ModifyCeiling(c) => vec![c.to_delta(graph)?],
            CommandKind::PlaceDoor(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::PlaceWindow(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::MoveOpening(c) => vec![c.to_delta(graph)?],
            CommandKind::DeleteOpening(c) => vec![c.to_delta(graph)?],
            CommandKind::PaintMaterial(c) => {
                c.validate()?;
                vec![c.to_delta(graph)?]
            }
            CommandKind::SwapFinish(c) => vec![c.to_paint().to_delta(graph)?],
            CommandKind::SetLighting(c) => {
                c.validate()?;
                // Lighting preset is captured as audit-only state; no graph delta.
                vec![]
            }
            CommandKind::AddLight(c) => vec![c.to_delta()],
            CommandKind::RemoveLight(c) => vec![c.to_delta(graph)?],
            CommandKind::SaveCamera(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::UpdateCamera(c) => vec![c.to_delta(graph)?],
            CommandKind::DeleteCamera(c) => vec![c.to_delta(graph)?],
            CommandKind::PlaceFurniture(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::MoveFurniture(c) => {
                c.validate()?;
                vec![c.to_delta(graph)?]
            }
            CommandKind::DeleteFurniture(c) => vec![c.to_delta(graph)?],

            // ----- Draft scope -----
            CommandKind::DrawPrimitive(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::EditTool(c) => {
                c.validate()?;
                c.to_deltas(graph)?
            }
            CommandKind::CreateSheet(c) => {
                c.validate()?;
                vec![c.to_delta()?]
            }
            CommandKind::SetLayerState(c) => {
                c.validate()?;
                vec![c.to_delta(graph)?]
            }

            // ----- Deliver scope -----
            //
            // `CreateRevision` captures the gesture in the audit chain
            // (via `execute_persistent`) but does not mutate the
            // project graph. The actual revision file is written by
            // the service layer; this command is the audit-trail hook.
            CommandKind::CreateRevision(c) => {
                c.validate()?;
                vec![]
            }
        })
    }

    /// Rebuild the engine from a SQLCipher-backed project package. The
    /// graph is loaded from the `entities` table and the journal from
    /// `undo_journal`. Use [`Self::execute_persistent`] to commit
    /// subsequent commands back to disk.
    pub fn open(conn: &rusqlite::Connection, active_scope: Scope) -> Result<Self> {
        let graph = crate::commands::ProjectGraph::load(conn)?;
        Self::open_with_graph(conn, active_scope, graph)
    }

    /// Like [`Self::open`] but reuses an already-loaded [`ProjectGraph`]
    /// instead of re-reading the `entities` table. The caller must
    /// guarantee `graph` reflects the current contents of `conn`'s
    /// `entities` table (i.e. nothing has mutated the table between
    /// loading the graph and this call). Used by the AI accept path
    /// and similar flows where the graph was loaded once for diff
    /// translation and would otherwise be re-read here.
    pub fn open_with_graph(
        conn: &rusqlite::Connection,
        active_scope: Scope,
        graph: crate::commands::ProjectGraph,
    ) -> Result<Self> {
        Ok(Self {
            graph,
            journal: crate::journal::UndoRedoJournal::load(conn, 1024)?,
            audit: crate::audit::AuditHashChain::new(),
            active_scope,
        })
    }

    /// Execute a command and persist the resulting deltas and journal
    /// entry to the connection.
    ///
    /// The pipeline is **all-or-nothing**:
    /// 1. Compute the forward deltas (pure, no mutation).
    /// 2. Validate the deltas against a clone of the graph so a
    ///    multi-delta command is checked end-to-end. Any failure here
    ///    returns immediately with both layers untouched.
    /// 3. Open a single SQL transaction; write every entity delta and
    ///    the journal entry inside it. If anything in the transaction
    ///    fails (or `commit()` itself fails) the tx is dropped and the
    ///    on-disk state is rolled back automatically.
    /// 4. **Only after** `commit()` succeeds do we mutate the in-memory
    ///    graph + journal. Because step 2 validated the deltas against
    ///    the current state, the in-memory apply is guaranteed to
    ///    succeed.
    ///
    /// Earlier iterations split entity writes and journal writes across
    /// separate transactions, which left a window where a `persist_undo`
    /// failure after a successful entity apply could leave the journal
    /// pointing at the wrong stack. The single-transaction shape closes
    /// that window.
    pub fn execute_persistent(
        &mut self,
        cmd: Command,
        conn: &mut rusqlite::Connection,
    ) -> Result<CommandResult> {
        let deltas = self.compute_deltas(&cmd.kind, &self.graph)?;
        self.graph.validate_all(&deltas)?;
        let inverse: Vec<EntityDelta> = deltas.iter().rev().map(EntityDelta::invert).collect();
        let entry = JournalEntry {
            command_id: cmd.command_id.clone(),
            applied_at: cmd.ts,
            // Tag the journal entry with the active scope so a
            // subsequent `undo` / `redo` can validate that the caller's
            // active scope matches the command's originating scope.
            scope: self.active_scope,
            forward: deltas.clone(),
            inverse,
        };
        let tx = conn.transaction()?;
        for d in &deltas {
            crate::commands::ProjectGraph::persist_delta_in_tx(&tx, d)?;
        }
        crate::journal::UndoRedoJournal::persist_record_in_tx(&tx, &entry)?;
        tx.commit()?;
        // SQL is committed atomically. Now mirror the changes in memory.
        // The applies cannot fail because validate_all succeeded against
        // the same starting state, and the engine holds a write lock
        // (see BridgeService's RwLock) so no concurrent mutation can
        // have invalidated the validation.
        for d in &deltas {
            self.graph
                .apply(d)
                .expect("validated above; apply cannot fail");
        }
        let envelope = self.audit.extend(
            &cmd.command_id,
            &serde_json::to_value(&cmd).unwrap_or(serde_json::Value::Null),
        );
        self.journal.record(entry);
        Ok(CommandResult {
            command_id: cmd.command_id,
            applied: deltas,
            audit: envelope,
        })
    }

    /// Execute a *batch* of commands inside a single SQL transaction.
    ///
    /// Phase 11 task 10 introduced this entry point: when the AI's
    /// `ai_accept_diff` converts a [`Diff`](aec_ai::Diff) into N
    /// typed commands, the renderer expects "apply all of these as
    /// one undo step" so a single Cmd-Z reverts the whole AI
    /// suggestion. Calling [`Self::execute_persistent`] N times in
    /// a loop would create N journal entries and N undo steps,
    /// which is the wrong UX shape and also performs N SQL commits.
    ///
    /// The same shape is used by bulk ingest paths (DXF import,
    /// IFC attach) where issuing N independent `execute_persistent`
    /// calls would mean N transactions, N audit envelope
    /// extensions, and N status-pane invalidations.
    ///
    /// Semantics:
    /// * The batch's scope must match the engine's `active_scope`.
    ///   This is the same contract enforced by
    ///   [`Self::execute_persistent`] via [`Self::compute_deltas`];
    ///   the batch path validates it once up front so a single
    ///   scope mismatch surfaces before any SQL is touched.
    /// * Every command in the batch must share the same scope as
    ///   command #0; any internally-inconsistent batch is rejected
    ///   with [`CommandError::ScopeMismatch`].
    /// * Deltas from every command are computed and validated
    ///   against a graph clone that walks forward through the
    ///   batch — so command #2's validation sees command #1's
    ///   inserts. This lets a batch like
    ///   `[create_wall_a, place_door_on_a]` validate cleanly.
    /// * All deltas are persisted in **one** `rusqlite::Transaction`.
    ///   Either every row commits or none do; there is no "applied
    ///   the first two but not the third" state observable from
    ///   outside this call.
    /// * The whole batch lands as **one** `JournalEntry` so a single
    ///   Cmd-Z reverts every command together. The merged entry's
    ///   `forward` is the concatenation of every command's deltas in
    ///   input order; its `inverse` is the concatenation of every
    ///   command's inverse deltas in **reverse** input order so
    ///   replaying it on undo undoes command #N first, then N-1,
    ///   etc. — matching the order required to roll back the
    ///   forward-walking validation graph. The entry is keyed by a
    ///   freshly-minted [`CommandId`] (`cmd_<uuid>`) because no
    ///   single per-command id represents the whole batch, and the
    ///   `applied_at` is the last command's timestamp (the moment
    ///   the user observed the batch land).
    /// * The audit-chain extension is still per-command (one
    ///   envelope per gesture in input order). The audit chain is
    ///   the forensic record of what the user/AI asked for; the
    ///   journal is the user-facing undo stack. Merging the
    ///   journal collapses *undo steps*, not the forensic trail.
    /// * On commit success the in-memory graph is updated in input
    ///   order and the merged journal entry is pushed once. The
    ///   result vector is in input order too so callers can
    ///   correlate `commands[i]` with `results[i]`; each result's
    ///   `applied` is that command's own forward deltas (a slice
    ///   of the merged entry).
    ///
    /// An empty batch is a no-op that returns an empty result
    /// vector and does not open a transaction.
    pub fn execute_persistent_batch(
        &mut self,
        commands: Vec<Command>,
        conn: &mut rusqlite::Connection,
    ) -> Result<Vec<CommandResult>> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        // Validate the entire batch BEFORE opening a transaction so
        // a malformed payload doesn't waste a SQLite write lock.
        //
        // (1) Active-scope guard. The single-command path enforces
        //     this implicitly through `compute_deltas`; the batch
        //     path enforces it explicitly so a misrouted batch
        //     (e.g. a Deliver-scope `CreateRevision` arriving on a
        //     Design engine) is rejected with one clear error
        //     instead of being decomposed per-command. This closes
        //     the gap Devin Review flagged as BUG_0001.
        let batch_scope = commands[0].scope;
        if batch_scope != self.active_scope {
            return Err(CommandError::ScopeMismatch {
                expected: self.active_scope.to_string(),
                actual: batch_scope.to_string(),
            });
        }
        // (2) Internal-consistency guard. Every command must agree
        //     with the batch's canonical scope; a mixed-scope batch
        //     would otherwise be impossible to journal cleanly (the
        //     `JournalEntry.scope` field is single-valued).
        for cmd in &commands {
            if cmd.scope != batch_scope {
                return Err(CommandError::ScopeMismatch {
                    expected: batch_scope.to_string(),
                    actual: cmd.scope.to_string(),
                });
            }
        }
        // Phase 1: pre-compute every command's deltas, validating
        // each step against a forward-walking clone so command #2
        // can see command #1's inserts. We collect everything up
        // front so the transaction window stays short — SQLite's
        // writer lock blocks every concurrent reader for the
        // duration.
        let mut staging = self.graph.clone();
        let mut staged: Vec<(Command, Vec<EntityDelta>, Vec<EntityDelta>)> =
            Vec::with_capacity(commands.len());
        for cmd in commands {
            // `compute_deltas_against_graph` is the pure form that
            // does NOT re-check scope; we already validated scope
            // for the entire batch above. Using this form avoids
            // re-running the scope guard per command and keeps the
            // error returned by phase 1 specifically about the
            // graph-level validation failure.
            let deltas = Self::compute_deltas_against_graph(&staging, &cmd.kind)?;
            staging.validate_all(&deltas)?;
            for d in &deltas {
                staging.apply(d)?;
            }
            let inverse: Vec<EntityDelta> = deltas.iter().rev().map(EntityDelta::invert).collect();
            staged.push((cmd, deltas, inverse));
        }
        // Build the single merged `JournalEntry` representing the
        // whole batch as one undo step. See the function docstring
        // for the inverse-order rationale (we apply per-command
        // inverses in reverse batch order on undo so the
        // forward-walking validation graph rolls back in lock-step).
        let merged_forward: Vec<EntityDelta> = staged
            .iter()
            .flat_map(|(_, fwd, _)| fwd.iter().cloned())
            .collect();
        let merged_inverse: Vec<EntityDelta> = staged
            .iter()
            .rev()
            .flat_map(|(_, _, inv)| inv.iter().cloned())
            .collect();
        let merged_entry = JournalEntry {
            command_id: CommandId::new(),
            applied_at: staged
                .last()
                .map(|(cmd, _, _)| cmd.ts)
                .expect("non-empty batch guard above ensures staged is non-empty"),
            scope: self.active_scope,
            forward: merged_forward,
            inverse: merged_inverse,
        };
        // Phase 2: single SQL transaction covering every delta and
        // the one merged journal entry. If any write fails (or
        // `commit()` itself fails) the batch is rolled back and
        // none of the in-memory mutations from phase 3 execute.
        let tx = conn.transaction()?;
        for d in &merged_entry.forward {
            crate::commands::ProjectGraph::persist_delta_in_tx(&tx, d)?;
        }
        crate::journal::UndoRedoJournal::persist_record_in_tx(&tx, &merged_entry)?;
        tx.commit()?;
        // Phase 3: SQL is committed atomically. Mirror in-memory
        // state, extend the audit chain per-command (preserving the
        // forensic per-gesture record), and build the per-command
        // result vector. We move `merged_entry` into the journal
        // at the end so the engine owns the entry for undo.
        for d in &merged_entry.forward {
            self.graph
                .apply(d)
                .expect("validated above; apply cannot fail");
        }
        let mut results = Vec::with_capacity(staged.len());
        for (cmd, deltas, _inv) in staged {
            let envelope = self.audit.extend(
                &cmd.command_id,
                &serde_json::to_value(&cmd).unwrap_or(serde_json::Value::Null),
            );
            results.push(CommandResult {
                command_id: cmd.command_id,
                applied: deltas,
                audit: envelope,
            });
        }
        self.journal.record(merged_entry);
        Ok(results)
    }

    /// Persistent counterpart to [`Self::undo`]. Same single-transaction
    /// validate -> SQL -> commit -> in-memory pipeline as
    /// [`Self::execute_persistent`]: peek the top entry without
    /// removing it, validate the inverse deltas, write everything in
    /// one tx, commit, and only then move the in-memory journal stacks.
    pub fn undo_persistent(&mut self, conn: &mut rusqlite::Connection) -> Result<CommandResult> {
        let entry = self
            .journal
            .peek_undo()
            .ok_or(CommandError::NothingToUndo)?
            .clone();
        // Reject before validating deltas or opening a tx so a scope
        // mismatch leaves both the journal and the SQL state untouched.
        if entry.scope != self.active_scope {
            return Err(CommandError::ScopeMismatch {
                expected: entry.scope.to_string(),
                actual: self.active_scope.to_string(),
            });
        }
        self.graph.validate_all(&entry.inverse)?;
        let tx = conn.transaction()?;
        for d in &entry.inverse {
            crate::commands::ProjectGraph::persist_delta_in_tx(&tx, d)?;
        }
        crate::journal::UndoRedoJournal::persist_undo_in_tx(&tx, &entry.command_id)?;
        tx.commit()?;
        for d in &entry.inverse {
            self.graph
                .apply(d)
                .expect("validated above; apply cannot fail");
        }
        let taken = self
            .journal
            .take_undo()
            .expect("peeked above; take_undo cannot return None");
        self.journal.commit_undone(taken.clone());
        let envelope = self.audit.extend(
            &taken.command_id,
            &serde_json::json!({"undo": taken.command_id.as_str()}),
        );
        Ok(CommandResult {
            command_id: taken.command_id,
            applied: taken.inverse,
            audit: envelope,
        })
    }

    /// Persistent counterpart to [`Self::redo`]. Mirrors
    /// [`Self::undo_persistent`].
    pub fn redo_persistent(&mut self, conn: &mut rusqlite::Connection) -> Result<CommandResult> {
        let entry = self
            .journal
            .peek_redo()
            .ok_or(CommandError::NothingToRedo)?
            .clone();
        // Mirror of the scope check in [`Self::undo_persistent`].
        if entry.scope != self.active_scope {
            return Err(CommandError::ScopeMismatch {
                expected: entry.scope.to_string(),
                actual: self.active_scope.to_string(),
            });
        }
        self.graph.validate_all(&entry.forward)?;
        let tx = conn.transaction()?;
        for d in &entry.forward {
            crate::commands::ProjectGraph::persist_delta_in_tx(&tx, d)?;
        }
        crate::journal::UndoRedoJournal::persist_redo_in_tx(&tx, &entry.command_id)?;
        tx.commit()?;
        for d in &entry.forward {
            self.graph
                .apply(d)
                .expect("validated above; apply cannot fail");
        }
        let taken = self
            .journal
            .take_redo()
            .expect("peeked above; take_redo cannot return None");
        self.journal.commit_redone(taken.clone());
        let envelope = self.audit.extend(
            &taken.command_id,
            &serde_json::json!({"redo": taken.command_id.as_str()}),
        );
        Ok(CommandResult {
            command_id: taken.command_id,
            applied: taken.forward,
            audit: envelope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::room::CreateRoom;
    use crate::commands::{camera, lighting, material, opening, wall, CommandKind};
    use aec_core::types::EntityId;

    fn wall_a() -> wall::CreateWall {
        wall::CreateWall {
            entity_id: EntityId::new(),
            start_mm: [0.0, 0.0],
            end_mm: [4500.0, 0.0],
            height_mm: 2700.0,
            thickness_mm: 100.0,
            material_id: None,
        }
    }

    #[test]
    fn execute_create_wall_grows_graph() {
        let mut e = CommandEngine::new(Scope::Design);
        let cmd = Command::user(CommandKind::CreateWall(wall_a()));
        let res = e.execute(cmd).unwrap();
        assert_eq!(res.applied.len(), 1);
        assert_eq!(e.graph().len(), 1);
    }

    #[test]
    fn undo_then_redo_restores_state() {
        let mut e = CommandEngine::new(Scope::Design);
        let create = Command::user(CommandKind::CreateWall(wall_a()));
        e.execute(create.clone()).unwrap();
        assert_eq!(e.graph().len(), 1);

        e.undo().unwrap();
        assert_eq!(e.graph().len(), 0);

        e.redo().unwrap();
        assert_eq!(e.graph().len(), 1);
    }

    #[test]
    fn invalid_wall_rejected_with_validation_error() {
        let mut e = CommandEngine::new(Scope::Design);
        let mut bad = wall_a();
        bad.height_mm = -1.0;
        let cmd = Command::user(CommandKind::CreateWall(bad));
        let err = e.execute(cmd).unwrap_err();
        match err {
            CommandError::InvalidArguments { .. } => {}
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn move_wall_updates_endpoints() {
        let mut e = CommandEngine::new(Scope::Design);
        let create = wall_a();
        let id = create.entity_id.clone();
        e.execute(Command::user(CommandKind::CreateWall(create)))
            .unwrap();
        let mv = wall::MoveWall {
            entity_id: id.clone(),
            new_start_mm: [10.0, 0.0],
            new_end_mm: [4500.0, 0.0],
        };
        e.execute(Command::user(CommandKind::MoveWall(mv))).unwrap();
        let body = &e.graph().get(&id).unwrap().body;
        assert_eq!(body["start_mm"], serde_json::json!([10.0, 0.0]));
    }

    #[test]
    fn dry_run_does_not_modify_graph() {
        let e = CommandEngine::new(Scope::Design);
        let kind = CommandKind::CreateWall(wall_a());
        let deltas = e.dry_run(&kind).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(e.graph().len(), 0);
    }

    #[test]
    fn place_door_creates_opening_parented_to_wall() {
        let mut e = CommandEngine::new(Scope::Design);
        let w = wall_a();
        let wall_id = w.entity_id.clone();
        e.execute(Command::user(CommandKind::CreateWall(w)))
            .unwrap();
        let door = opening::PlaceDoor {
            entity_id: EntityId::new(),
            host_wall_id: wall_id.clone(),
            position_along_wall_mm: 1000.0,
            width_mm: 900.0,
            height_mm: 2100.0,
            door_kind: opening::DoorKind::SingleSwing,
        };
        let door_id = door.entity_id.clone();
        e.execute(Command::user(CommandKind::PlaceDoor(door)))
            .unwrap();
        let rec = e.graph().get(&door_id).unwrap();
        assert_eq!(rec.kind, "door");
        assert_eq!(rec.parent, Some(wall_id));
    }

    #[test]
    fn undo_on_empty_journal_returns_error() {
        let mut e = CommandEngine::new(Scope::Design);
        let err = e.undo().unwrap_err();
        matches!(err, CommandError::NothingToUndo);
    }

    #[test]
    fn failed_undo_keeps_entry_on_undo_stack() {
        // Regression test for the two-phase journal invariant:
        // if `apply_deltas` fails during `undo()`, the journal entry must
        // stay on the undo stack rather than being silently shunted to the
        // redo stack.
        let mut e = CommandEngine::new(Scope::Design);
        let create = wall_a();
        let wall_id = create.entity_id.clone();
        e.execute(Command::user(CommandKind::CreateWall(create)))
            .unwrap();
        assert_eq!(e.undo_len(), 1);

        // Externally remove the wall so the inverse delta (Delete{record})
        // will fail with EntityNotFound when undo() replays it.
        let record = e.graph().get(&wall_id).cloned().unwrap();
        e.graph_mut()
            .apply(&crate::commands::EntityDelta::Delete { record })
            .unwrap();
        assert_eq!(e.graph().len(), 0);

        let err = e.undo().unwrap_err();
        assert!(matches!(err, CommandError::EntityNotFound(_)));
        // Journal stays consistent: entry is back on undo, redo is empty.
        assert_eq!(e.undo_len(), 1);
        assert_eq!(e.redo_len(), 0);
    }

    #[test]
    fn failed_redo_keeps_entry_on_redo_stack() {
        // Symmetric to `failed_undo_keeps_entry_on_undo_stack`.
        let mut e = CommandEngine::new(Scope::Design);
        let create = wall_a();
        let wall_id = create.entity_id.clone();
        e.execute(Command::user(CommandKind::CreateWall(create)))
            .unwrap();
        e.undo().unwrap();
        assert_eq!(e.redo_len(), 1);

        // After the undo above, the graph is empty. Externally re-creating
        // the wall makes the forward delta (Create) fail with
        // EntityAlreadyExists when redo() replays it.
        let record = crate::commands::EntityRecord {
            id: wall_id.clone(),
            kind: "wall".into(),
            body: serde_json::json!({"sentinel": true}),
            parent: None,
        };
        e.graph_mut()
            .apply(&crate::commands::EntityDelta::Create { record })
            .unwrap();

        let err = e.redo().unwrap_err();
        assert!(matches!(err, CommandError::EntityAlreadyExists(_)));
        assert_eq!(e.redo_len(), 1);
        assert_eq!(e.undo_len(), 0);
    }

    #[test]
    fn create_room_validates_minimum_walls() {
        let mut e = CommandEngine::new(Scope::Design);
        let cmd = Command::user(CommandKind::CreateRoom(CreateRoom {
            entity_id: EntityId::new(),
            name: "Living".into(),
            wall_ids: vec![EntityId::new(), EntityId::new()],
            floor_id: None,
            ceiling_id: None,
        }));
        let err = e.execute(cmd).unwrap_err();
        matches!(err, CommandError::InvalidArguments { .. });
    }

    #[test]
    fn paint_material_validates_id() {
        let mut e = CommandEngine::new(Scope::Design);
        // Create a wall first
        let w = wall_a();
        let wid = w.entity_id.clone();
        e.execute(Command::user(CommandKind::CreateWall(w)))
            .unwrap();

        // Empty material id -> rejected
        let bad = material::PaintMaterial {
            target_entity_id: wid.clone(),
            material_id: "  ".into(),
            surface: None,
        };
        let err = e
            .execute(Command::user(CommandKind::PaintMaterial(bad)))
            .unwrap_err();
        matches!(err, CommandError::InvalidArguments { .. });

        // Valid paint applies
        let ok = material::PaintMaterial {
            target_entity_id: wid.clone(),
            material_id: "mat_oak".into(),
            surface: None,
        };
        e.execute(Command::user(CommandKind::PaintMaterial(ok)))
            .unwrap();
        let body = &e.graph().get(&wid).unwrap().body;
        assert_eq!(body["material_id"], serde_json::json!("mat_oak"));
    }

    #[test]
    fn save_camera_records_in_graph() {
        let mut e = CommandEngine::new(Scope::Design);
        let cam = camera::SaveCamera {
            entity_id: EntityId::new(),
            name: "Hero".into(),
            params: camera::CameraParams {
                position_mm: [3000.0, -2000.0, 1500.0],
                target_mm: [0.0, 0.0, 1200.0],
                focal_length_mm: 35.0,
                exposure_ev: 0.0,
                white_balance_k: 5500,
                depth_of_field_f: Some(2.8),
                aspect_ratio: 16.0 / 9.0,
            },
        };
        let id = cam.entity_id.clone();
        e.execute(Command::user(CommandKind::SaveCamera(cam)))
            .unwrap();
        let rec = e.graph().get(&id).unwrap();
        assert_eq!(rec.kind, "camera");
    }

    #[test]
    fn add_light_then_remove_light_roundtrips() {
        let mut e = CommandEngine::new(Scope::Design);
        let light_cmd = lighting::AddLight {
            entity_id: EntityId::new(),
            light: lighting::LightKind::Point {
                position_mm: [1000.0, 1000.0, 2500.0],
                intensity: 800.0,
                color_temperature_k: 3000,
            },
        };
        let id = light_cmd.entity_id.clone();
        e.execute(Command::user(CommandKind::AddLight(light_cmd)))
            .unwrap();
        assert!(e.graph().get(&id).is_some());

        e.execute(Command::user(CommandKind::RemoveLight(
            lighting::RemoveLight {
                entity_id: id.clone(),
            },
        )))
        .unwrap();
        assert!(e.graph().get(&id).is_none());
    }

    #[test]
    fn set_lighting_records_audit_only() {
        let mut e = CommandEngine::new(Scope::Design);
        let cmd = Command::user(CommandKind::SetLighting(lighting::SetLighting {
            preset_id: "warm_evening".into(),
        }));
        let res = e.execute(cmd).unwrap();
        assert!(res.applied.is_empty()); // no graph delta but audit envelope still emitted
        assert!(res.audit.hash.starts_with("blake3:"));
    }

    // -------- persistent-path rollback regression tests --------

    fn open_in_memory_persistent_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Mirror the persistent schema that `aec_core::db::open_encrypted`
        // installs. We only need the columns the engine touches.
        conn.execute_batch(
            "CREATE TABLE entities (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                parent_id   TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                body        TEXT NOT NULL
            );
            CREATE TABLE undo_journal (
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
    fn execute_persistent_batch_applies_all_or_nothing() {
        use crate::commands::draft::DrawPrimitive;
        use aec_cad::primitives::{Line, Primitive};
        // Three independent DrawPrimitive commands. The batch must:
        // (a) end with all three entities persisted in `entities`,
        // (b) record **one** merged journal entry (single Cmd-Z
        //     reverts the whole batch),
        // (c) leave the engine's in-memory graph + journal in
        //     lock-step with the persisted state.
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Draft).unwrap();
        let mk = |x: f64| {
            Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                entity_id: EntityId::new(),
                primitive: Primitive::Line(Line::new("0", [x, 0.0], [x + 10.0, 0.0])),
            }))
        };
        let cmds = vec![mk(0.0), mk(20.0), mk(40.0)];
        let results = e.execute_persistent_batch(cmds, &mut conn).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(e.graph().len(), 3);
        // One undo step for the whole batch (single Cmd-Z reverts
        // every command together).
        assert_eq!(e.undo_len(), 1);
        let entity_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        let journal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM undo_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entity_count, 3);
        assert_eq!(journal_count, 1);
    }

    #[test]
    fn execute_persistent_batch_rejects_duplicate_inside_batch() {
        use crate::commands::draft::DrawPrimitive;
        use aec_cad::primitives::{Line, Primitive};
        // Two commands that both target the **same** entity_id. The
        // second one collides with the first inside the same batch,
        // so phase-1 validation must reject the whole batch and
        // *no* rows / journal entries should be persisted.
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Draft).unwrap();
        let shared_id = EntityId::new();
        let mk = |id: EntityId| {
            Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                entity_id: id,
                primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
            }))
        };
        let cmds = vec![mk(shared_id.clone()), mk(shared_id)];
        let err = e.execute_persistent_batch(cmds, &mut conn).unwrap_err();
        assert!(matches!(err, CommandError::EntityAlreadyExists(_)));
        // All-or-nothing: neither command persisted.
        assert_eq!(e.graph().len(), 0);
        assert_eq!(e.undo_len(), 0);
        let entity_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entity_count, 0);
    }

    #[test]
    fn execute_persistent_batch_resolves_cross_command_dependencies() {
        // Regression for the BUG-0001 finding: `compute_deltas` used to
        // read from `self.graph`, which is never updated during the
        // batch loop. A later command depending on an entity created by
        // an earlier command in the *same* batch (e.g. EditTool::Move
        // targeting a primitive just drawn by DrawPrimitive) would fail
        // with EntityNotFound. After the fix, `compute_deltas` accepts
        // an explicit `&ProjectGraph` and the batch path passes the
        // forward-running shadow, so the dependency resolves.
        use crate::commands::draft::{DrawPrimitive, EditOperation, EditTool};
        use aec_cad::primitives::{Line, Primitive};
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Draft).unwrap();
        let id = EntityId::new();
        let draw = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        }));
        let mv = Command::user(CommandKind::EditTool(EditTool {
            operation: EditOperation::Move {
                entity_ids: vec![id.clone()],
                dx: 5.0,
                dy: 5.0,
            },
        }));
        let results = e
            .execute_persistent_batch(vec![draw, mv], &mut conn)
            .unwrap();
        assert_eq!(results.len(), 2);
        // Both commands persisted; both in-memory deltas applied.
        assert_eq!(e.graph().len(), 1);
        // The whole batch is one undo step (draw + move reverts as
        // a single Cmd-Z so the user-perceived gesture is atomic).
        assert_eq!(e.undo_len(), 1);
        // The moved primitive should be at the translated position.
        let record = e.graph().get(&id).unwrap();
        let dp = serde_json::from_value::<DrawPrimitive>(record.body.clone()).unwrap();
        if let Primitive::Line(line) = dp.primitive {
            assert_eq!(line.start, [5.0, 5.0]);
            assert_eq!(line.end, [15.0, 5.0]);
        } else {
            panic!("expected Line primitive after Move");
        }
    }

    #[test]
    fn execute_persistent_batch_empty_input_is_noop() {
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Draft).unwrap();
        let results = e.execute_persistent_batch(vec![], &mut conn).unwrap();
        assert!(results.is_empty());
        assert_eq!(e.graph().len(), 0);
        assert_eq!(e.undo_len(), 0);
    }

    #[test]
    fn execute_persistent_batch_rejects_scope_mismatch_with_active_scope() {
        // Regression for Devin Review BUG_0001: the batch path used to
        // only check that every command in the batch shared a scope
        // with command #0, but never validated that the batch's scope
        // matched the engine's `active_scope`. The single-command
        // path enforces this implicitly through `compute_deltas` and
        // the batch path now enforces it explicitly as a single
        // up-front check. A misrouted batch (Design engine receiving
        // a Draft-scope batch) must be rejected with `ScopeMismatch`
        // BEFORE any SQL is touched, BEFORE deltas are computed, and
        // without partially mutating either layer.
        use crate::commands::draft::DrawPrimitive;
        use aec_cad::primitives::{Line, Primitive};
        let mut conn = open_in_memory_persistent_db();
        // Engine opened in Design scope.
        let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
        // Batch of two Draft-scope commands. Internal consistency
        // holds (both are Draft) but the batch as a whole disagrees
        // with the engine's active scope.
        let cmds = vec![
            Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                entity_id: EntityId::new(),
                primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
            })),
            Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                entity_id: EntityId::new(),
                primitive: Primitive::Line(Line::new("0", [10.0, 0.0], [20.0, 0.0])),
            })),
        ];
        let err = e.execute_persistent_batch(cmds, &mut conn).unwrap_err();
        match err {
            CommandError::ScopeMismatch { expected, actual } => {
                assert_eq!(expected, Scope::Design.to_string());
                assert_eq!(actual, Scope::Draft.to_string());
            }
            other => panic!("expected ScopeMismatch, got {other:?}"),
        }
        // All-or-nothing: nothing persisted, nothing in-memory.
        assert_eq!(e.graph().len(), 0);
        assert_eq!(e.undo_len(), 0);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 0);
        let journal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM undo_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal_count, 0);
    }

    #[test]
    fn execute_persistent_batch_rejects_internally_inconsistent_scope() {
        // Companion test for BUG_0001 second-stage guard: even when
        // command #0 matches the engine's active scope, a batch where
        // command #N has a different scope than command #0 must be
        // rejected (mixed-scope batches can't be cleanly journaled
        // since `JournalEntry.scope` is single-valued).
        use crate::commands::draft::DrawPrimitive;
        use aec_cad::primitives::{Line, Primitive};
        let mut conn = open_in_memory_persistent_db();
        // Engine opened in Draft scope (matches command #0).
        let mut e = CommandEngine::open(&conn, Scope::Draft).unwrap();
        // Command #0 is correctly Draft; we then hand-construct a
        // second Command with `scope: Design` (artificial, since
        // `Command::user(kind)` derives scope from `kind.scope()` —
        // this exercises the explicit guard that protects the
        // journal even if a caller bypasses the constructor).
        let mut bad = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: EntityId::new(),
            primitive: Primitive::Line(Line::new("0", [10.0, 0.0], [20.0, 0.0])),
        }));
        bad.scope = Scope::Design;
        let cmds = vec![
            Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
                entity_id: EntityId::new(),
                primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
            })),
            bad,
        ];
        let err = e.execute_persistent_batch(cmds, &mut conn).unwrap_err();
        match err {
            CommandError::ScopeMismatch { expected, actual } => {
                assert_eq!(expected, Scope::Draft.to_string());
                assert_eq!(actual, Scope::Design.to_string());
            }
            other => panic!("expected ScopeMismatch, got {other:?}"),
        }
        assert_eq!(e.graph().len(), 0);
        assert_eq!(e.undo_len(), 0);
    }

    #[test]
    fn execute_persistent_validation_failure_leaves_both_layers_untouched() {
        // Pre-condition: a wall already in the DB. A second CreateWall
        // with the **same** entity_id is a validation error — execute_persistent
        // must reject it before opening any SQL transaction, so the
        // entities row count stays at exactly 1 and the journal at 0.
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
        let w = wall_a();
        let cmd = Command::user(CommandKind::CreateWall(w.clone()));
        e.execute_persistent(cmd, &mut conn).unwrap();
        assert_eq!(e.graph().len(), 1);

        // Second create with the same entity_id.
        let duplicate = Command::user(CommandKind::CreateWall(w));
        let err = e.execute_persistent(duplicate, &mut conn).unwrap_err();
        assert!(matches!(err, CommandError::EntityAlreadyExists(_)));

        // Neither layer mutated: in-memory still 1 entity + 1 journal entry,
        // SQL still 1 row + 1 journal record.
        assert_eq!(e.graph().len(), 1);
        assert_eq!(e.undo_len(), 1);
        let entity_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        let journal_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM undo_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entity_count, 1);
        assert_eq!(journal_count, 1);
    }

    #[test]
    fn undo_persistent_validation_failure_leaves_journal_untouched() {
        // Persistent undo replays the inverse deltas; if the in-memory
        // graph already lost the entity (e.g., schema drift or external
        // edit), the validation must fail and the journal entry must
        // remain on the undo stack — exactly mirroring the in-memory
        // `failed_undo_keeps_entry_on_undo_stack` invariant.
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
        let w = wall_a();
        let wall_id = w.entity_id.clone();
        e.execute_persistent(Command::user(CommandKind::CreateWall(w)), &mut conn)
            .unwrap();
        assert_eq!(e.undo_len(), 1);

        // Externally remove the entity from the in-memory graph so the
        // inverse Delete fails validation. (We don't touch the DB so
        // the row stays — that's fine for the validation test.)
        let record = e.graph().get(&wall_id).cloned().unwrap();
        e.graph_mut()
            .apply(&crate::commands::EntityDelta::Delete { record })
            .unwrap();

        let err = e.undo_persistent(&mut conn).unwrap_err();
        assert!(matches!(err, CommandError::EntityNotFound(_)));
        // Journal entry remains on undo (peek-based: never taken).
        assert_eq!(e.undo_len(), 1);
        assert_eq!(e.redo_len(), 0);
        // SQL journal record's `superseded` flag is still 0.
        let superseded: i64 = conn
            .query_row("SELECT superseded FROM undo_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(superseded, 0);
    }

    #[test]
    fn persistent_round_trip_executes_undoes_and_redoes() {
        let mut conn = open_in_memory_persistent_db();
        let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
        let w = wall_a();
        let wall_id = w.entity_id.clone();
        e.execute_persistent(Command::user(CommandKind::CreateWall(w)), &mut conn)
            .unwrap();
        assert_eq!(e.graph().len(), 1);

        e.undo_persistent(&mut conn).unwrap();
        assert_eq!(e.graph().len(), 0);
        assert_eq!(e.undo_len(), 0);
        assert_eq!(e.redo_len(), 1);
        let superseded: i64 = conn
            .query_row("SELECT superseded FROM undo_journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(superseded, 1);

        e.redo_persistent(&mut conn).unwrap();
        assert_eq!(e.graph().len(), 1);
        assert!(e.graph().contains(&wall_id));
        assert_eq!(e.undo_len(), 1);
        assert_eq!(e.redo_len(), 0);
    }

    #[test]
    fn draft_scope_rejects_design_command() {
        let mut e = CommandEngine::new(Scope::Draft);
        let cmd = Command::user(CommandKind::CreateWall(wall_a()));
        let err = e.execute(cmd).unwrap_err();
        assert!(matches!(err, CommandError::ScopeMismatch { .. }));
    }

    #[test]
    fn draft_scope_routes_draw_primitive() {
        use crate::commands::draft::DrawPrimitive;
        use aec_cad::primitives::{Line, Primitive};

        let mut e = CommandEngine::new(Scope::Draft);
        let id = EntityId::new();
        let cmd = Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        }));
        let res = e.execute(cmd).unwrap();
        assert_eq!(res.applied.len(), 1);
        let rec = e.graph().get(&id).unwrap();
        assert_eq!(rec.kind, "primitive");
    }

    #[test]
    fn draft_scope_edit_tool_translates_primitive() {
        use crate::commands::draft::{DrawPrimitive, EditOperation, EditTool};
        use aec_cad::primitives::{Line, Primitive};

        let mut e = CommandEngine::new(Scope::Draft);
        let id = EntityId::new();
        e.execute(Command::user(CommandKind::DrawPrimitive(DrawPrimitive {
            entity_id: id.clone(),
            primitive: Primitive::Line(Line::new("0", [0.0, 0.0], [10.0, 0.0])),
        })))
        .unwrap();
        let edit = EditTool {
            operation: EditOperation::Move {
                entity_ids: vec![id.clone()],
                dx: 5.0,
                dy: 0.0,
            },
        };
        e.execute(Command::user(CommandKind::EditTool(edit)))
            .unwrap();
        let body = &e.graph().get(&id).unwrap().body;
        let primitive: DrawPrimitive = serde_json::from_value(body.clone()).unwrap();
        if let Primitive::Line(l) = primitive.primitive {
            assert_eq!(l.start, [5.0, 0.0]);
            assert_eq!(l.end, [15.0, 0.0]);
        } else {
            panic!("expected line");
        }
    }

    #[test]
    fn draft_scope_create_sheet_inserts_sheet_entity() {
        use crate::commands::draft::CreateSheet;
        use aec_cad::sheets::{Margins, Orientation, PaperSize};

        let mut e = CommandEngine::new(Scope::Draft);
        let id = EntityId::new();
        e.execute(Command::user(CommandKind::CreateSheet(CreateSheet {
            entity_id: id.clone(),
            name: "A-101".into(),
            paper: PaperSize::IsoA3,
            orientation: Orientation::Landscape,
            margins: Margins::default(),
            title_block: None,
            viewports: vec![],
        })))
        .unwrap();
        let rec = e.graph().get(&id).unwrap();
        assert_eq!(rec.kind, "sheet");
    }

    #[test]
    fn draft_scope_set_layer_state_upserts() {
        use crate::commands::draft::SetLayerState;
        use aec_cad::layers::{LayerColor, LayerLineweight};

        let mut e = CommandEngine::new(Scope::Draft);
        let id = EntityId::new();
        // First call: create.
        e.execute(Command::user(CommandKind::SetLayerState(SetLayerState {
            entity_id: id.clone(),
            name: "WALLS".into(),
            color: Some(LayerColor(1)),
            linetype: Some("CONTINUOUS".into()),
            lineweight: Some(LayerLineweight::from_mm(0.5)),
            on: Some(true),
            frozen: Some(false),
            locked: Some(false),
            plottable: Some(true),
            description: None,
        })))
        .unwrap();
        // Second call: update only `frozen`.
        e.execute(Command::user(CommandKind::SetLayerState(SetLayerState {
            entity_id: id.clone(),
            name: "WALLS".into(),
            color: None,
            linetype: None,
            lineweight: None,
            on: None,
            frozen: Some(true),
            locked: None,
            plottable: None,
            description: None,
        })))
        .unwrap();
        let layer: aec_cad::layers::Layer =
            serde_json::from_value(e.graph().get(&id).unwrap().body.clone()).unwrap();
        assert!(layer.frozen);
        assert_eq!(layer.name, "WALLS");
        assert_eq!(layer.color, LayerColor(1));
    }

    #[test]
    fn deliver_scope_create_revision_is_audit_only() {
        use crate::commands::deliver::CreateRevision;

        let mut e = CommandEngine::new(Scope::Deliver);
        let before = e.graph().len();
        let res = e
            .execute(Command::user(CommandKind::CreateRevision(CreateRevision {
                tag: "r1".into(),
                description: "first snapshot".into(),
                revision_id: None,
            })))
            .unwrap();
        // No graph delta — revision capture is audit-only.
        assert!(res.applied.is_empty());
        assert_eq!(e.graph().len(), before);
        // Undo should still work (it just unwinds the journal entry).
        e.undo().unwrap();
    }

    #[test]
    fn persistent_apply_survives_engine_reopen() {
        // The point of the persistent path: after we drop the engine the
        // state survives. Reopening from the same connection rebuilds an
        // engine whose graph + journal match the pre-drop state.
        let mut conn = open_in_memory_persistent_db();
        let wall_id = {
            let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
            let w = wall_a();
            let id = w.entity_id.clone();
            e.execute_persistent(Command::user(CommandKind::CreateWall(w)), &mut conn)
                .unwrap();
            id
        };
        let e2 = CommandEngine::open(&conn, Scope::Design).unwrap();
        assert_eq!(e2.graph().len(), 1);
        assert!(e2.graph().contains(&wall_id));
        assert_eq!(e2.undo_len(), 1);
        assert_eq!(e2.redo_len(), 0);
    }

    #[test]
    fn phase12_task28_undo_redo_stacks_survive_simulated_crash() {
        // Phase 12 Task 28 — apply 3 commands, undo 2, redo 1, then
        // drop the engine (simulating a process crash) and reopen.
        // The undo and redo stack sizes (and their entity IDs) must
        // exactly match the pre-crash state.
        let mut conn = open_in_memory_persistent_db();
        let mut wall_ids: Vec<EntityId> = Vec::new();

        // Apply three create-wall commands. Each shifts the wall to a
        // distinct location so the graph state has three independent
        // entities and the journal three distinct entries.
        {
            let mut e = CommandEngine::open(&conn, Scope::Design).unwrap();
            for offset_mm in [0.0_f64, 5000.0, 10_000.0] {
                let w = wall::CreateWall {
                    entity_id: EntityId::new(),
                    start_mm: [offset_mm, 0.0],
                    end_mm: [offset_mm + 4500.0, 0.0],
                    height_mm: 2700.0,
                    thickness_mm: 100.0,
                    material_id: None,
                };
                wall_ids.push(w.entity_id.clone());
                e.execute_persistent(Command::user(CommandKind::CreateWall(w)), &mut conn)
                    .unwrap();
            }
            assert_eq!(e.graph().len(), 3);
            assert_eq!(e.undo_len(), 3);
            assert_eq!(e.redo_len(), 0);

            // Undo twice — graph drops to 1 wall, undo→1, redo→2.
            e.undo_persistent(&mut conn).unwrap();
            e.undo_persistent(&mut conn).unwrap();
            assert_eq!(e.graph().len(), 1);
            assert_eq!(e.undo_len(), 1);
            assert_eq!(e.redo_len(), 2);

            // Redo once — graph back to 2 walls, undo→2, redo→1.
            e.redo_persistent(&mut conn).unwrap();
            assert_eq!(e.graph().len(), 2);
            assert_eq!(e.undo_len(), 2);
            assert_eq!(e.redo_len(), 1);
        } // <-- engine drops here, simulating a crash.

        // Reopen from the connection. Stack depths AND entity IDs must
        // match the pre-crash snapshot.
        let restarted = CommandEngine::open(&conn, Scope::Design).unwrap();
        assert_eq!(restarted.graph().len(), 2);
        assert_eq!(restarted.undo_len(), 2);
        assert_eq!(restarted.redo_len(), 1);
        // The first two walls must be present; the third (still on the
        // redo stack) must not.
        assert!(restarted.graph().contains(&wall_ids[0]));
        assert!(restarted.graph().contains(&wall_ids[1]));
        assert!(!restarted.graph().contains(&wall_ids[2]));
    }
}
