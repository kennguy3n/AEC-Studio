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
            forward: applied.clone(),
            inverse,
        });
        Ok(CommandResult { command_id: cmd.command_id, applied, audit: envelope })
    }

    /// Return the deltas this command *would* produce without mutating.
    pub fn dry_run(&self, kind: &CommandKind) -> Result<Vec<EntityDelta>> {
        self.compute_deltas(kind)
    }

    /// Undo the most recently executed command.
    pub fn undo(&mut self) -> Result<CommandResult> {
        let entry = self.journal.pop_undo().ok_or(CommandError::NothingToUndo)?;
        let applied = self.apply_deltas(&entry.inverse)?;
        let envelope = self.audit.extend(
            &entry.command_id,
            &serde_json::json!({"undo": entry.command_id.as_str()}),
        );
        Ok(CommandResult { command_id: entry.command_id, applied, audit: envelope })
    }

    /// Redo the most recently undone command.
    pub fn redo(&mut self) -> Result<CommandResult> {
        let entry = self.journal.pop_redo().ok_or(CommandError::NothingToRedo)?;
        let applied = self.apply_deltas(&entry.forward)?;
        let envelope = self.audit.extend(
            &entry.command_id,
            &serde_json::json!({"redo": entry.command_id.as_str()}),
        );
        Ok(CommandResult { command_id: entry.command_id, applied, audit: envelope })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandKind;
    use crate::commands::room::CreateRoom;
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
        e.execute(Command::user(CommandKind::CreateWall(create))).unwrap();
        let mv = wall::MoveWall { entity_id: id.clone(), new_start_mm: [10.0, 0.0], new_end_mm: [4500.0, 0.0] };
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
        e.execute(Command::user(CommandKind::CreateWall(w))).unwrap();
        let door = opening::PlaceDoor {
            entity_id: EntityId::new(),
            host_wall_id: wall_id.clone(),
            position_along_wall_mm: 1000.0,
            width_mm: 900.0,
            height_mm: 2100.0,
            door_kind: opening::DoorKind::SingleSwing,
        };
        let door_id = door.entity_id.clone();
        e.execute(Command::user(CommandKind::PlaceDoor(door))).unwrap();
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
        e.execute(Command::user(CommandKind::CreateWall(w))).unwrap();

        // Empty material id -> rejected
        let bad = material::PaintMaterial {
            target_entity_id: wid.clone(),
            material_id: "  ".into(),
            surface: None,
        };
        let err = e.execute(Command::user(CommandKind::PaintMaterial(bad))).unwrap_err();
        matches!(err, CommandError::InvalidArguments { .. });

        // Valid paint applies
        let ok = material::PaintMaterial {
            target_entity_id: wid.clone(),
            material_id: "mat_oak".into(),
            surface: None,
        };
        e.execute(Command::user(CommandKind::PaintMaterial(ok))).unwrap();
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
        e.execute(Command::user(CommandKind::SaveCamera(cam))).unwrap();
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
        e.execute(Command::user(CommandKind::AddLight(light_cmd))).unwrap();
        assert!(e.graph().get(&id).is_some());

        e.execute(Command::user(CommandKind::RemoveLight(lighting::RemoveLight {
            entity_id: id.clone(),
        })))
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
}
