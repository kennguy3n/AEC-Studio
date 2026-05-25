//! Typed commands for AEC Studio.

pub mod camera;
pub mod deliver;
pub mod draft;
pub mod floor;
pub mod furniture;
pub mod lighting;
pub mod material;
pub mod opening;
pub mod project_graph;
pub mod room;
pub mod wall;

use serde::{Deserialize, Serialize};

use aec_core::types::{Actor, CommandId, EntityId, Scope};

pub use project_graph::{EntityDelta, EntityRecord, ProjectGraph};

/// Every concrete command kind known to the engine. New command types must
/// be added here so the engine can dispatch them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "arguments")]
pub enum CommandKind {
    #[serde(rename = "design.create_wall")]
    CreateWall(wall::CreateWall),
    #[serde(rename = "design.move_wall")]
    MoveWall(wall::MoveWall),
    #[serde(rename = "design.delete_wall")]
    DeleteWall(wall::DeleteWall),

    #[serde(rename = "design.create_room")]
    CreateRoom(room::CreateRoom),
    #[serde(rename = "design.modify_room")]
    ModifyRoom(room::ModifyRoom),

    #[serde(rename = "design.create_floor")]
    CreateFloor(floor::CreateFloor),
    #[serde(rename = "design.modify_floor")]
    ModifyFloor(floor::ModifyFloor),

    #[serde(rename = "design.place_door")]
    PlaceDoor(opening::PlaceDoor),
    #[serde(rename = "design.place_window")]
    PlaceWindow(opening::PlaceWindow),
    #[serde(rename = "design.move_opening")]
    MoveOpening(opening::MoveOpening),
    #[serde(rename = "design.delete_opening")]
    DeleteOpening(opening::DeleteOpening),

    #[serde(rename = "design.paint_material")]
    PaintMaterial(material::PaintMaterial),
    #[serde(rename = "design.swap_finish")]
    SwapFinish(material::SwapFinish),

    #[serde(rename = "design.set_lighting")]
    SetLighting(lighting::SetLighting),
    #[serde(rename = "design.add_light")]
    AddLight(lighting::AddLight),
    #[serde(rename = "design.remove_light")]
    RemoveLight(lighting::RemoveLight),

    #[serde(rename = "design.save_camera")]
    SaveCamera(camera::SaveCamera),
    #[serde(rename = "design.update_camera")]
    UpdateCamera(camera::UpdateCamera),
    #[serde(rename = "design.delete_camera")]
    DeleteCamera(camera::DeleteCamera),

    #[serde(rename = "design.place_furniture")]
    PlaceFurniture(furniture::PlaceFurniture),
    #[serde(rename = "design.move_furniture")]
    MoveFurniture(furniture::MoveFurniture),
    #[serde(rename = "design.delete_furniture")]
    DeleteFurniture(furniture::DeleteFurniture),

    // ----- Draft scope -----
    #[serde(rename = "draft.draw_primitive")]
    DrawPrimitive(draft::DrawPrimitive),
    #[serde(rename = "draft.edit_tool")]
    EditTool(draft::EditTool),
    #[serde(rename = "draft.create_sheet")]
    CreateSheet(draft::CreateSheet),
    #[serde(rename = "draft.set_layer_state")]
    SetLayerState(draft::SetLayerState),

    // ----- Deliver scope -----
    #[serde(rename = "deliver.create_revision")]
    CreateRevision(deliver::CreateRevision),
}

impl CommandKind {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::CreateWall(_) => "design.create_wall",
            Self::MoveWall(_) => "design.move_wall",
            Self::DeleteWall(_) => "design.delete_wall",
            Self::CreateRoom(_) => "design.create_room",
            Self::ModifyRoom(_) => "design.modify_room",
            Self::CreateFloor(_) => "design.create_floor",
            Self::ModifyFloor(_) => "design.modify_floor",
            Self::PlaceDoor(_) => "design.place_door",
            Self::PlaceWindow(_) => "design.place_window",
            Self::MoveOpening(_) => "design.move_opening",
            Self::DeleteOpening(_) => "design.delete_opening",
            Self::PaintMaterial(_) => "design.paint_material",
            Self::SwapFinish(_) => "design.swap_finish",
            Self::SetLighting(_) => "design.set_lighting",
            Self::AddLight(_) => "design.add_light",
            Self::RemoveLight(_) => "design.remove_light",
            Self::SaveCamera(_) => "design.save_camera",
            Self::UpdateCamera(_) => "design.update_camera",
            Self::DeleteCamera(_) => "design.delete_camera",
            Self::PlaceFurniture(_) => "design.place_furniture",
            Self::MoveFurniture(_) => "design.move_furniture",
            Self::DeleteFurniture(_) => "design.delete_furniture",
            Self::DrawPrimitive(_) => "draft.draw_primitive",
            Self::EditTool(_) => "draft.edit_tool",
            Self::CreateSheet(_) => "draft.create_sheet",
            Self::SetLayerState(_) => "draft.set_layer_state",
            Self::CreateRevision(_) => "deliver.create_revision",
        }
    }

    /// Active scope of this command. Phase 2 commands all sit in the
    /// `Design` scope; Phase 3 (`Draft`) and the Deliver-mode revision
    /// commands fan out into their own scopes so the engine's scope-
    /// matching check (`engine.rs::compute_deltas`) refuses to apply
    /// a `draft.*` command while the engine is in `Design` and vice
    /// versa.
    pub fn scope(&self) -> Scope {
        match self {
            Self::CreateWall(_)
            | Self::MoveWall(_)
            | Self::DeleteWall(_)
            | Self::CreateRoom(_)
            | Self::ModifyRoom(_)
            | Self::CreateFloor(_)
            | Self::ModifyFloor(_)
            | Self::PlaceDoor(_)
            | Self::PlaceWindow(_)
            | Self::MoveOpening(_)
            | Self::DeleteOpening(_)
            | Self::PaintMaterial(_)
            | Self::SwapFinish(_)
            | Self::SetLighting(_)
            | Self::AddLight(_)
            | Self::RemoveLight(_)
            | Self::SaveCamera(_)
            | Self::UpdateCamera(_)
            | Self::DeleteCamera(_)
            | Self::PlaceFurniture(_)
            | Self::MoveFurniture(_)
            | Self::DeleteFurniture(_) => Scope::Design,
            Self::DrawPrimitive(_)
            | Self::EditTool(_)
            | Self::CreateSheet(_)
            | Self::SetLayerState(_) => Scope::Draft,
            Self::CreateRevision(_) => Scope::Deliver,
        }
    }
}

/// A fully-qualified command record carrying provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    pub command_id: CommandId,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub scope: Scope,
    pub actor: Actor,
    #[serde(flatten)]
    pub kind: CommandKind,
}

impl Command {
    pub fn user(kind: CommandKind) -> Self {
        Self {
            command_id: CommandId::new(),
            ts: chrono::Utc::now(),
            scope: kind.scope(),
            actor: Actor::user(),
            kind,
        }
    }

    pub fn ai(tool: impl Into<String>, kind: CommandKind) -> Self {
        Self {
            command_id: CommandId::new(),
            ts: chrono::Utc::now(),
            scope: kind.scope(),
            actor: Actor::ai(tool),
            kind,
        }
    }
}

/// Identifier shared between commands and entity deltas.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub id: EntityId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Room,
    Wall,
    Floor,
    World,
}
