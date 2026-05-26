//! Template instantiation -> typed [`Command`]s.
//!
//! The shipped templates (`templates/interior/apartment.json`,
//! `templates/architecture/villa.json`, etc.) declare rooms, default wall
//! thicknesses, a lighting preset, an asset shelf, and camera presets.
//! This module walks a [`TemplateDefinition`] and produces the sequence of
//! [`Command`]s that materialise that template into real entities on the
//! project graph.
//!
//! ## What gets created per room
//!
//! For every room in the template (single-storey flat list OR every
//! room across every storey), the module emits:
//!
//! * **4 wall entities** — one per side (south, east, north, west).
//!   Each wall carries the template-defined `interior_thickness_mm` for
//!   the three shared sides and `exterior_thickness_mm` for the south
//!   wall (the typical exterior facade in a strip layout).
//! * **1 floor entity** — a rectangular boundary polygon equal to the
//!   room's footprint, with a 50 mm finish thickness.
//! * **1 ceiling entity** — same boundary as the floor, with a 30 mm
//!   finish thickness. The room links to it via `ceiling_id`.
//! * **1 room entity** — wires up the wall ids + floor id + ceiling id
//!   into a single logical room.
//!
//! On top of that, the module emits:
//!
//! * **1 `SetLighting` command** if the template carries a
//!   `lighting_preset` (it's audit-only state with no graph delta).
//! * **N `SaveCamera` commands** — one per `camera_presets` entry,
//!   sensible defaults filled in for the fields the JSON doesn't carry
//!   (exposure, white balance, aspect ratio).
//!
//! ## Layout strategy
//!
//! Template rooms don't carry an `origin_mm` (all shipped templates omit
//! it). To produce non-overlapping geometry we lay rooms out in a
//! horizontal strip — each room starts at the east edge of the previous
//! room, with a small `LAYOUT_GAP_MM` gap to keep adjacent rooms from
//! sharing wall lines. This is deterministic and reproducible, which is
//! all the test suite needs to verify "real geometry, not stubs".
//!
//! Multi-storey templates (e.g. `architecture.villa`) lay out each
//! storey on its own strip, with each storey starting at `x=0` —
//! storeys live at different `elevation_mm` so 2D overlap is fine.
//!
//! ## Fail-soft semantics
//!
//! The module returns an [`InstantiationOutcome`] that always succeeds,
//! even if a template room has zero dimensions or the lighting preset is
//! `None`. Problematic rooms are recorded in
//! `InstantiationOutcome::skipped` instead of being silently dropped.
//! Drafting templates (`drafting.2d_drafting`) have an empty rooms list
//! and emit zero geometry commands; their `sheet_presets` and
//! `dim_styles` are sheet-level state and are handled elsewhere
//! (`deliver_*` commands).

use aec_core::templates::{TemplateDefinition, TemplateRoom, TemplateStorey};
use aec_core::types::{Actor, EntityId};

use crate::commands::camera::{CameraParams, SaveCamera};
use crate::commands::ceiling::CreateCeiling;
use crate::commands::floor::CreateFloor;
use crate::commands::lighting::SetLighting;
use crate::commands::room::CreateRoom;
use crate::commands::wall::CreateWall;
use crate::commands::{Command, CommandKind};

/// Spacing inserted between adjacent rooms in the strip layout. Wide
/// enough that two adjacent room footprints don't share a wall line
/// (which would make geometry merging ambiguous downstream), narrow
/// enough that a multi-room template fits comfortably in a sensible
/// workspace footprint.
pub const LAYOUT_GAP_MM: f64 = 100.0;

/// Floor finish thickness in mm. Shipped templates don't override this,
/// so we pin a sensible architectural default (≈50 mm covers a screed
/// + wood/tile finish stack).
pub const DEFAULT_FLOOR_THICKNESS_MM: f64 = 50.0;

/// Ceiling finish thickness in mm. Smaller than the floor — a typical
/// suspended ceiling is ≈30 mm including the gypsum board.
pub const DEFAULT_CEILING_THICKNESS_MM: f64 = 30.0;

/// Default ceiling material. Shipped templates' `default_walls.material`
/// covers walls only.
pub const DEFAULT_CEILING_MATERIAL: &str = "ceiling_white";

/// Default floor material. Same rationale as ceiling.
pub const DEFAULT_FLOOR_MATERIAL: &str = "floor_default";

/// Sensible defaults for camera fields the template JSON doesn't
/// carry. Templates only declare `location_mm`, `target_mm`, and
/// `focal_length_mm`; the rest is exposed for follow-up tweaks via
/// `design.update_camera`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraDefaults {
    pub exposure_ev: f64,
    pub white_balance_k: u32,
    pub aspect_ratio: f64,
}

impl Default for CameraDefaults {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            white_balance_k: 5600,
            aspect_ratio: 16.0 / 9.0,
        }
    }
}

/// Bookkeeping for a single materialised room. Returned so callers
/// (tests, the bridge service's `project_create_from_template`) can
/// correlate template rooms to the entities that landed on the graph.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomInstantiation {
    pub room_id: EntityId,
    /// Four walls in clockwise order from the south wall: S, E, N, W.
    pub wall_ids: [EntityId; 4],
    pub floor_id: EntityId,
    pub ceiling_id: EntityId,
    /// `Some(storey_name)` if this room came from a multi-storey
    /// template, `None` if it came from the flat `rooms` list.
    pub storey_name: Option<String>,
    /// South-west corner of the room's footprint in mm.
    pub footprint_origin_mm: [f64; 2],
    /// `[width, depth]` of the room's footprint in mm. Echoes the
    /// template values verbatim so tests can match against the JSON.
    pub footprint_size_mm: [f64; 2],
    /// Storey-relative ceiling height in mm. Echoes
    /// `TemplateRoom.height_mm` verbatim.
    pub height_mm: f64,
}

/// A template room the instantiator could NOT materialise (e.g. a
/// zero-size footprint). The skip reason is human-readable and is
/// suitable for an audit log entry. Returning these instead of
/// returning an `Err` keeps the "create project from template" path
/// fail-soft: a single bad room cannot brick project creation for a
/// template that has many other valid rooms.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedRoom {
    pub storey_name: Option<String>,
    pub room_name: String,
    pub reason: String,
}

/// The full outcome of converting a template into commands. The
/// `commands` vector is in dispatch order and can be fed directly to
/// [`crate::engine::CommandEngine::execute_persistent_batch`].
#[derive(Debug, Clone, PartialEq)]
pub struct InstantiationOutcome {
    pub commands: Vec<Command>,
    pub rooms: Vec<RoomInstantiation>,
    pub camera_ids: Vec<EntityId>,
    pub lighting_preset: Option<String>,
    pub skipped: Vec<SkippedRoom>,
}

impl InstantiationOutcome {
    pub fn entity_count(&self) -> usize {
        // 7 entities per room: 4 walls + 1 floor + 1 ceiling + 1 room
        // (the room itself is also an entity). The lighting `SetLighting`
        // command is audit-only and produces no entity.
        self.rooms.len() * 7 + self.camera_ids.len()
    }
}

/// Convert a parsed [`TemplateDefinition`] into a batch of
/// `Actor::user()`-tagged commands. Use this when the user picks a
/// template from the project-create dialog.
///
/// Use [`template_to_commands_as`] when you need a different actor (e.g.
/// `Actor::ai("template_assist")` for AI-suggested template fills, or
/// `Actor::external("kchat:#proj-42")` for chat-driven instantiation).
pub fn template_to_commands(template: &TemplateDefinition) -> InstantiationOutcome {
    template_to_commands_as(template, Actor::user(), CameraDefaults::default())
}

/// As [`template_to_commands`] but with caller-supplied actor + camera
/// defaults. Pulled out so tests can pin the actor for assertion.
pub fn template_to_commands_as(
    template: &TemplateDefinition,
    actor: Actor,
    cam_defaults: CameraDefaults,
) -> InstantiationOutcome {
    let mut commands: Vec<Command> = Vec::new();
    let mut rooms_out: Vec<RoomInstantiation> = Vec::new();
    let mut camera_ids: Vec<EntityId> = Vec::new();
    let mut skipped: Vec<SkippedRoom> = Vec::new();

    let ext = template.default_walls.exterior_thickness_mm;
    let int = template.default_walls.interior_thickness_mm;
    let wall_material = template.default_walls.material.clone();

    // Flat rooms (single-storey templates).
    layout_storey(
        &template.rooms,
        None,
        ext,
        int,
        wall_material.as_deref(),
        &actor,
        &mut commands,
        &mut rooms_out,
        &mut skipped,
    );

    // Multi-storey rooms (villa etc.). Each storey lays out
    // independently on its own strip starting at x=0; storeys live
    // at different `elevation_mm` so 2D overlap is fine.
    for storey in &template.storeys {
        layout_storey(
            &storey.rooms,
            Some(storey),
            ext,
            int,
            wall_material.as_deref(),
            &actor,
            &mut commands,
            &mut rooms_out,
            &mut skipped,
        );
    }

    // Lighting preset (one command, no graph delta).
    if let Some(preset_id) = &template.lighting_preset {
        commands.push(actor_command(
            &actor,
            CommandKind::SetLighting(SetLighting {
                preset_id: preset_id.clone(),
            }),
        ));
    }

    // Cameras.
    for cam in &template.camera_presets {
        let camera_id = EntityId::new();
        camera_ids.push(camera_id.clone());
        let cmd = SaveCamera {
            entity_id: camera_id,
            name: cam.name.clone(),
            params: CameraParams {
                position_mm: cam.location_mm,
                target_mm: cam.target_mm,
                focal_length_mm: cam.focal_length_mm,
                exposure_ev: cam_defaults.exposure_ev,
                white_balance_k: cam_defaults.white_balance_k,
                depth_of_field_f: None,
                aspect_ratio: cam_defaults.aspect_ratio,
            },
        };
        commands.push(actor_command(&actor, CommandKind::SaveCamera(cmd)));
    }

    InstantiationOutcome {
        commands,
        rooms: rooms_out,
        camera_ids,
        lighting_preset: template.lighting_preset.clone(),
        skipped,
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_storey(
    rooms: &[TemplateRoom],
    storey: Option<&TemplateStorey>,
    exterior_thickness_mm: f64,
    interior_thickness_mm: f64,
    wall_material: Option<&str>,
    actor: &Actor,
    commands: &mut Vec<Command>,
    rooms_out: &mut Vec<RoomInstantiation>,
    skipped: &mut Vec<SkippedRoom>,
) {
    let storey_name = storey.map(|s| s.name.clone());
    let mut cursor_x = 0.0_f64;

    for room in rooms {
        if let Some(reason) = validate_room(room) {
            skipped.push(SkippedRoom {
                storey_name: storey_name.clone(),
                room_name: room.name.clone(),
                reason,
            });
            continue;
        }

        let origin_x = if room.origin_mm == [0.0, 0.0, 0.0] {
            cursor_x
        } else {
            room.origin_mm[0]
        };
        let origin_y = if room.origin_mm == [0.0, 0.0, 0.0] {
            0.0
        } else {
            room.origin_mm[1]
        };

        let width = room.width_mm;
        let depth = room.depth_mm;
        let height = room.height_mm;

        // Footprint corners in clockwise order from the SW corner.
        let sw = [origin_x, origin_y];
        let se = [origin_x + width, origin_y];
        let ne = [origin_x + width, origin_y + depth];
        let nw = [origin_x, origin_y + depth];

        // Four walls. South is exterior (typical strip-layout south
        // facade), the other three are interior. Each wall is its
        // own entity so we get four ids back.
        let wall_south = wall(
            sw,
            se,
            height,
            exterior_thickness_mm,
            wall_material,
            actor,
            commands,
        );
        let wall_east = wall(
            se,
            ne,
            height,
            interior_thickness_mm,
            wall_material,
            actor,
            commands,
        );
        let wall_north = wall(
            ne,
            nw,
            height,
            interior_thickness_mm,
            wall_material,
            actor,
            commands,
        );
        let wall_west = wall(
            nw,
            sw,
            height,
            interior_thickness_mm,
            wall_material,
            actor,
            commands,
        );

        // Floor.
        let floor_id = EntityId::new();
        commands.push(actor_command(
            actor,
            CommandKind::CreateFloor(CreateFloor {
                entity_id: floor_id.clone(),
                boundary_mm: vec![sw, se, ne, nw],
                thickness_mm: DEFAULT_FLOOR_THICKNESS_MM,
                material_id: Some(DEFAULT_FLOOR_MATERIAL.to_string()),
            }),
        ));

        // Ceiling.
        let ceiling_id = EntityId::new();
        commands.push(actor_command(
            actor,
            CommandKind::CreateCeiling(CreateCeiling {
                entity_id: ceiling_id.clone(),
                boundary_mm: vec![sw, se, ne, nw],
                thickness_mm: DEFAULT_CEILING_THICKNESS_MM,
                material_id: Some(DEFAULT_CEILING_MATERIAL.to_string()),
            }),
        ));

        // Room linking the four walls + floor + ceiling.
        let room_id = EntityId::new();
        let wall_ids = [wall_south, wall_east, wall_north, wall_west];
        commands.push(actor_command(
            actor,
            CommandKind::CreateRoom(CreateRoom {
                entity_id: room_id.clone(),
                name: room.name.clone(),
                wall_ids: wall_ids.to_vec(),
                floor_id: Some(floor_id.clone()),
                ceiling_id: Some(ceiling_id.clone()),
            }),
        ));

        rooms_out.push(RoomInstantiation {
            room_id,
            wall_ids,
            floor_id,
            ceiling_id,
            storey_name: storey_name.clone(),
            footprint_origin_mm: [origin_x, origin_y],
            footprint_size_mm: [width, depth],
            height_mm: height,
        });

        cursor_x = origin_x + width + LAYOUT_GAP_MM;
    }
}

fn wall(
    start_mm: [f64; 2],
    end_mm: [f64; 2],
    height_mm: f64,
    thickness_mm: f64,
    material: Option<&str>,
    actor: &Actor,
    commands: &mut Vec<Command>,
) -> EntityId {
    let id = EntityId::new();
    let cmd = CreateWall {
        entity_id: id.clone(),
        start_mm,
        end_mm,
        height_mm,
        thickness_mm,
        material_id: material.map(std::string::ToString::to_string),
    };
    commands.push(actor_command(actor, CommandKind::CreateWall(cmd)));
    id
}

fn actor_command(actor: &Actor, kind: CommandKind) -> Command {
    match actor.kind {
        aec_core::types::ActorKind::User => Command::user(kind),
        aec_core::types::ActorKind::Ai => {
            let tool = actor.tool.clone().unwrap_or_else(|| "ai".to_string());
            Command::ai(tool, kind)
        }
        aec_core::types::ActorKind::KChat => {
            // Templates are always user-initiated today, so a KChat
            // actor would only arrive via a forced override. `Command`
            // has no dedicated KChat constructor; fall back to a user
            // command so audit-trail integrity is preserved. The
            // KChat actor handle is still available on the parent
            // operation's audit envelope.
            Command::user(kind)
        }
    }
}

fn validate_room(room: &TemplateRoom) -> Option<String> {
    if room.width_mm <= 0.0 {
        return Some(format!("width_mm must be > 0 (got {})", room.width_mm));
    }
    if room.depth_mm <= 0.0 {
        return Some(format!("depth_mm must be > 0 (got {})", room.depth_mm));
    }
    if room.height_mm <= 0.0 {
        return Some(format!("height_mm must be > 0 (got {})", room.height_mm));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::templates::{TemplateLoader, WallDefaults};
    use aec_core::types::{ActorKind, Region, Units};
    use std::collections::BTreeMap;

    fn flat_template_with_three_rooms() -> TemplateDefinition {
        let mut region_defaults = BTreeMap::new();
        region_defaults.insert(
            Region::Eu,
            aec_core::templates::RegionDefaults {
                units: Units::Mm,
                standards: vec!["IFC4".into()],
            },
        );
        TemplateDefinition {
            template_id: "interior.test".into(),
            category: Some("interior".into()),
            name: "Test".into(),
            description: "test".into(),
            region_defaults,
            units: Units::Mm,
            rooms: vec![
                TemplateRoom {
                    name: "Living".into(),
                    width_mm: 5000.0,
                    depth_mm: 4000.0,
                    height_mm: 2700.0,
                    origin_mm: [0.0, 0.0, 0.0],
                },
                TemplateRoom {
                    name: "Bedroom".into(),
                    width_mm: 3500.0,
                    depth_mm: 3500.0,
                    height_mm: 2700.0,
                    origin_mm: [0.0, 0.0, 0.0],
                },
                TemplateRoom {
                    name: "Bathroom".into(),
                    width_mm: 2000.0,
                    depth_mm: 2000.0,
                    height_mm: 2700.0,
                    origin_mm: [0.0, 0.0, 0.0],
                },
            ],
            storeys: vec![],
            default_walls: WallDefaults {
                exterior_thickness_mm: 250.0,
                interior_thickness_mm: 100.0,
                material: Some("wall_white".into()),
            },
            lighting_preset: Some("warm_evening".into()),
            asset_shelf: vec![],
            camera_presets: vec![aec_core::templates::TemplateCamera {
                name: "Hero".into(),
                location_mm: [3000.0, -2000.0, 1700.0],
                target_mm: [0.0, 0.0, 1500.0],
                focal_length_mm: 35.0,
            }],
            sheet_presets: vec![],
            dim_styles: vec![],
        }
    }

    #[test]
    fn flat_template_produces_one_room_set_per_room() {
        let tpl = flat_template_with_three_rooms();
        let out = template_to_commands(&tpl);
        // 3 rooms -> 12 walls + 3 floors + 3 ceilings + 3 rooms = 21
        // entity-creating commands, plus 1 SetLighting (no entity) and
        // 1 SaveCamera (one entity) = 23 total commands.
        let mut wall_count = 0;
        let mut floor_count = 0;
        let mut ceiling_count = 0;
        let mut room_count = 0;
        let mut light_count = 0;
        let mut camera_count = 0;
        for cmd in &out.commands {
            match &cmd.kind {
                CommandKind::CreateWall(_) => wall_count += 1,
                CommandKind::CreateFloor(_) => floor_count += 1,
                CommandKind::CreateCeiling(_) => ceiling_count += 1,
                CommandKind::CreateRoom(_) => room_count += 1,
                CommandKind::SetLighting(_) => light_count += 1,
                CommandKind::SaveCamera(_) => camera_count += 1,
                _ => {}
            }
        }
        assert_eq!(wall_count, 12, "expected 4 walls per room x 3 rooms");
        assert_eq!(floor_count, 3);
        assert_eq!(ceiling_count, 3);
        assert_eq!(room_count, 3);
        assert_eq!(light_count, 1);
        assert_eq!(camera_count, 1);

        assert_eq!(out.rooms.len(), 3);
        assert_eq!(out.skipped.len(), 0);
        assert_eq!(out.lighting_preset.as_deref(), Some("warm_evening"));
    }

    #[test]
    fn rooms_lay_out_east_of_each_other() {
        let tpl = flat_template_with_three_rooms();
        let out = template_to_commands(&tpl);
        let r = &out.rooms;
        // Room 0 starts at x=0; room 1 starts at x = w0 + gap; room 2
        // starts at x = w0 + gap + w1 + gap.
        assert_eq!(r[0].footprint_origin_mm, [0.0, 0.0]);
        assert!((r[1].footprint_origin_mm[0] - (5000.0 + LAYOUT_GAP_MM)).abs() < 1e-9);
        assert!(
            (r[2].footprint_origin_mm[0] - (5000.0 + LAYOUT_GAP_MM + 3500.0 + LAYOUT_GAP_MM)).abs()
                < 1e-9
        );
        // Footprint size echoes JSON width/depth.
        assert_eq!(r[0].footprint_size_mm, [5000.0, 4000.0]);
        assert_eq!(r[1].footprint_size_mm, [3500.0, 3500.0]);
        assert_eq!(r[2].footprint_size_mm, [2000.0, 2000.0]);
    }

    #[test]
    fn user_actor_is_attributed_on_emitted_commands() {
        let tpl = flat_template_with_three_rooms();
        let out = template_to_commands(&tpl);
        for cmd in &out.commands {
            assert_eq!(cmd.actor.kind, ActorKind::User);
        }
    }

    #[test]
    fn ai_actor_is_attributed_when_caller_passes_it() {
        let tpl = flat_template_with_three_rooms();
        let out = template_to_commands_as(
            &tpl,
            Actor::ai("template_assist"),
            CameraDefaults::default(),
        );
        for cmd in &out.commands {
            assert_eq!(cmd.actor.kind, ActorKind::Ai);
            assert_eq!(cmd.actor.tool.as_deref(), Some("template_assist"));
        }
    }

    #[test]
    fn rooms_link_walls_floor_and_ceiling() {
        let tpl = flat_template_with_three_rooms();
        let out = template_to_commands(&tpl);
        let create_rooms: Vec<&CreateRoom> = out
            .commands
            .iter()
            .filter_map(|c| match &c.kind {
                CommandKind::CreateRoom(r) => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(create_rooms.len(), 3);
        for (cr, ri) in create_rooms.iter().zip(out.rooms.iter()) {
            assert_eq!(cr.wall_ids.len(), 4);
            assert!(cr.floor_id.is_some());
            assert!(cr.ceiling_id.is_some());
            assert_eq!(cr.floor_id.as_ref(), Some(&ri.floor_id));
            assert_eq!(cr.ceiling_id.as_ref(), Some(&ri.ceiling_id));
            assert_eq!(cr.wall_ids, ri.wall_ids.to_vec());
        }
    }

    #[test]
    fn drafting_template_emits_zero_geometry_commands() {
        let mut tpl = flat_template_with_three_rooms();
        tpl.rooms.clear();
        tpl.lighting_preset = None;
        tpl.camera_presets.clear();
        let out = template_to_commands(&tpl);
        assert_eq!(out.commands.len(), 0);
        assert_eq!(out.rooms.len(), 0);
        assert_eq!(out.camera_ids.len(), 0);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn zero_size_rooms_are_skipped_not_failing() {
        let mut tpl = flat_template_with_three_rooms();
        tpl.rooms.push(TemplateRoom {
            name: "BadRoom".into(),
            width_mm: 0.0,
            depth_mm: 1000.0,
            height_mm: 2700.0,
            origin_mm: [0.0, 0.0, 0.0],
        });
        let out = template_to_commands(&tpl);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].room_name, "BadRoom");
        assert!(out.skipped[0].reason.contains("width_mm"));
        // 3 valid rooms still produce their 7 entity-creating commands each.
        assert_eq!(out.rooms.len(), 3);
    }

    #[test]
    fn multi_storey_template_lays_out_each_storey_independently() {
        let mut tpl = flat_template_with_three_rooms();
        tpl.rooms.clear();
        tpl.storeys.push(TemplateStorey {
            name: "Ground".into(),
            elevation_mm: 0.0,
            rooms: vec![
                TemplateRoom {
                    name: "Foyer".into(),
                    width_mm: 3000.0,
                    depth_mm: 3000.0,
                    height_mm: 2700.0,
                    origin_mm: [0.0, 0.0, 0.0],
                },
                TemplateRoom {
                    name: "Living".into(),
                    width_mm: 5000.0,
                    depth_mm: 4000.0,
                    height_mm: 2700.0,
                    origin_mm: [0.0, 0.0, 0.0],
                },
            ],
        });
        tpl.storeys.push(TemplateStorey {
            name: "Upper".into(),
            elevation_mm: 3000.0,
            rooms: vec![TemplateRoom {
                name: "Master".into(),
                width_mm: 4000.0,
                depth_mm: 4000.0,
                height_mm: 2700.0,
                origin_mm: [0.0, 0.0, 0.0],
            }],
        });
        let out = template_to_commands(&tpl);
        // 3 rooms total -> 21 entity-creating commands + cam + light
        assert_eq!(out.rooms.len(), 3);
        assert_eq!(out.rooms[0].storey_name.as_deref(), Some("Ground"));
        assert_eq!(out.rooms[1].storey_name.as_deref(), Some("Ground"));
        assert_eq!(out.rooms[2].storey_name.as_deref(), Some("Upper"));
        // Each storey starts at x=0.
        assert_eq!(out.rooms[0].footprint_origin_mm, [0.0, 0.0]);
        assert!((out.rooms[1].footprint_origin_mm[0] - (3000.0 + LAYOUT_GAP_MM)).abs() < 1e-9);
        assert_eq!(out.rooms[2].footprint_origin_mm, [0.0, 0.0]);
    }

    fn templates_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("templates")
    }

    /// Round-trip every shipped template through the instantiator. The
    /// invariants checked are:
    ///
    /// * Every template parses cleanly.
    /// * Every room-bearing template (8 of the 9 shipped templates as
    ///   of writing) produces at least one room.
    /// * The room-less templates (drafting + renovation overlay) emit
    ///   zero geometry — they are sheet-only / layer-only workflows.
    /// * Every emitted SaveCamera / SetLighting matches the template's
    ///   declared `camera_presets` / `lighting_preset`.
    #[test]
    fn all_shipped_templates_produce_real_geometry() {
        let loader = TemplateLoader::new(templates_root());
        let keys = loader.discover().expect("discover templates");
        let mut had_geometry_templates = 0;
        let mut had_geometryless_templates = 0;
        for key in &keys {
            let tpl = loader
                .load(key)
                .unwrap_or_else(|e| panic!("failed to load template {key}: {e}"));
            let has_declared_rooms =
                !tpl.rooms.is_empty() || tpl.storeys.iter().any(|s| !s.rooms.is_empty());
            let out = template_to_commands(&tpl);
            if !has_declared_rooms {
                // Sheet-only / layer-only templates (drafting, renovation
                // overlay) must emit zero entity-creating commands.
                had_geometryless_templates += 1;
                let geom_cmd_count = out
                    .commands
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.kind,
                            CommandKind::CreateWall(_)
                                | CommandKind::CreateFloor(_)
                                | CommandKind::CreateCeiling(_)
                                | CommandKind::CreateRoom(_)
                        )
                    })
                    .count();
                assert_eq!(
                    geom_cmd_count, 0,
                    "geometry-less template {key} should not emit walls / floors / ceilings / rooms"
                );
                continue;
            }
            // Geometry templates must produce at least one materialised
            // room (none should silently land in `skipped` because of
            // bad shipped data).
            had_geometry_templates += 1;
            assert!(!out.rooms.is_empty(), "template {key} produced no rooms");
            assert!(
                out.skipped.is_empty(),
                "template {key} silently skipped rooms: {:?}",
                out.skipped
            );
            // Every room must have its full wall+floor+ceiling set.
            for ri in &out.rooms {
                assert_eq!(ri.wall_ids.len(), 4, "{key}: room missing walls");
                assert!(ri.height_mm > 0.0, "{key}: room height_mm must be positive");
                assert!(
                    ri.footprint_size_mm[0] > 0.0,
                    "{key}: room width must be positive"
                );
                assert!(
                    ri.footprint_size_mm[1] > 0.0,
                    "{key}: room depth must be positive"
                );
            }
            // Every camera_presets entry must produce a SaveCamera command.
            let cam_cmd_count = out
                .commands
                .iter()
                .filter(|c| matches!(c.kind, CommandKind::SaveCamera(_)))
                .count();
            assert_eq!(
                cam_cmd_count,
                tpl.camera_presets.len(),
                "{key}: camera count mismatch"
            );
            // Lighting preset emits exactly one SetLighting command.
            let light_cmd_count = out
                .commands
                .iter()
                .filter(|c| matches!(c.kind, CommandKind::SetLighting(_)))
                .count();
            let expected_light = usize::from(tpl.lighting_preset.is_some());
            assert_eq!(
                light_cmd_count, expected_light,
                "{key}: lighting command count mismatch"
            );
        }
        assert!(
            had_geometry_templates >= 7,
            "expected >= 7 geometry-bearing templates, got {had_geometry_templates}"
        );
        assert!(
            had_geometryless_templates >= 1,
            "expected at least one geometry-less template (drafting / renovation overlay)"
        );
    }
}
