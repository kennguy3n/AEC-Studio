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
        let deltas = self.compute_deltas(&cmd.kind)?;
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
        self.compute_deltas(kind)
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

    fn compute_deltas(&self, kind: &CommandKind) -> Result<Vec<EntityDelta>> {
        if kind.scope() != self.active_scope {
            return Err(CommandError::ScopeMismatch {
                expected: kind.scope().to_string(),
                actual: self.active_scope.to_string(),
            });
        }
        Ok(match kind {
            CommandKind::CreateWall(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::MoveWall(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::DeleteWall(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::CreateRoom(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::ModifyRoom(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::CreateFloor(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::ModifyFloor(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::PlaceDoor(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::PlaceWindow(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::MoveOpening(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::DeleteOpening(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::PaintMaterial(c) => {
                c.validate()?;
                vec![c.to_delta(&self.graph)?]
            }
            CommandKind::SwapFinish(c) => vec![c.to_paint().to_delta(&self.graph)?],
            CommandKind::SetLighting(c) => {
                c.validate()?;
                // Lighting preset is captured as audit-only state; no graph delta.
                vec![]
            }
            CommandKind::AddLight(c) => vec![c.to_delta()],
            CommandKind::RemoveLight(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::SaveCamera(c) => {
                c.validate()?;
                vec![c.to_delta()]
            }
            CommandKind::UpdateCamera(c) => vec![c.to_delta(&self.graph)?],
            CommandKind::DeleteCamera(c) => vec![c.to_delta(&self.graph)?],
        })
    }

    /// Rebuild the engine from a SQLCipher-backed project package. The
    /// graph is loaded from the `entities` table and the journal from
    /// `undo_journal`. Use [`Self::execute_persistent`] to commit
    /// subsequent commands back to disk.
    pub fn open(conn: &rusqlite::Connection, active_scope: Scope) -> Result<Self> {
        Ok(Self {
            graph: crate::commands::ProjectGraph::load(conn)?,
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
        let deltas = self.compute_deltas(&cmd.kind)?;
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

    /// Persistent counterpart to [`Self::undo`]. Same single-transaction
    /// validate → SQL → commit → in-memory pipeline as
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
}
