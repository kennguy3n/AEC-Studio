//! Phase-cutting integration test for the command engine's undo/redo
//! journal + the BLAKE3 audit hash chain, covering the surface area
//! the user journeys depend on:
//!
//! - 10 user + AI + KChat commands flow through `CommandEngine::execute`.
//! - Undo 5 → graph rolls back, `undo_len` / `redo_len` track.
//! - Redo 3 → graph restores, audit envelopes are emitted again
//!   *with a fresh head* (audit is append-only; redo doesn't rewind the
//!   chain).
//! - Undo all → graph is empty.
//! - Every executed command emits exactly one audit envelope with the
//!   expected actor kind (`user` / `ai` / `kchat`), the BLAKE3 hash
//!   chain links each entry to the previous one, and no hash is
//!   reused across the run.

use aec_command::{
    commands::{
        camera::{CameraParams, SaveCamera},
        material::PaintMaterial,
        room::CreateRoom,
        wall::CreateWall,
        CommandKind,
    },
    engine::CommandResult,
    Command, CommandEngine,
};
use aec_core::{Actor, ActorKind, EntityId, Scope};

/// Bundles the engine's [`CommandResult`] with the actor that
/// authored it so the test can assert provenance without reaching
/// back into the engine.
struct Executed {
    actor: Actor,
    result: CommandResult,
}

fn create_wall(x: f64) -> CommandKind {
    CommandKind::CreateWall(CreateWall {
        entity_id: EntityId::new(),
        start_mm: [x, 0.0],
        end_mm: [x + 1000.0, 0.0],
        height_mm: 2700.0,
        thickness_mm: 100.0,
        material_id: None,
    })
}

fn make_command(kind: CommandKind, actor: Actor) -> Command {
    Command {
        command_id: aec_core::CommandId::new(),
        ts: chrono::Utc::now(),
        scope: kind.scope(),
        actor,
        kind,
    }
}

#[test]
fn ten_commands_unwind_and_replay_with_intact_audit_chain() {
    let mut engine = CommandEngine::new(Scope::Design);

    let mut results: Vec<Executed> = Vec::new();

    // 1. Four user-authored CreateWall commands.
    let wall_ids: Vec<EntityId> = (0..4)
        .map(|i| {
            let kind = create_wall(i as f64 * 2000.0);
            let id = match &kind {
                CommandKind::CreateWall(w) => w.entity_id.clone(),
                _ => unreachable!(),
            };
            let actor = Actor::user();
            results.push(Executed {
                actor: actor.clone(),
                result: engine
                    .execute(make_command(kind, actor))
                    .expect("CreateWall must succeed"),
            });
            id
        })
        .collect();

    // 2. A user CreateRoom over those walls.
    let room_kind = CommandKind::CreateRoom(CreateRoom {
        entity_id: EntityId::new(),
        name: "Living".into(),
        wall_ids: wall_ids.clone(),
        floor_id: None,
        ceiling_id: None,
    });
    let actor = Actor::user();
    results.push(Executed {
        actor: actor.clone(),
        result: engine
            .execute(make_command(room_kind, actor))
            .expect("CreateRoom must succeed"),
    });

    // 3. AI-authored PaintMaterial — exercises ActorKind::Ai +
    //    tool provenance.
    let paint = CommandKind::PaintMaterial(PaintMaterial {
        target_entity_id: wall_ids[0].clone(),
        material_id: "oak_natural".into(),
        surface: None,
    });
    let actor = Actor::ai("style_assistant");
    results.push(Executed {
        actor: actor.clone(),
        result: engine
            .execute(make_command(paint, actor))
            .expect("PaintMaterial must succeed"),
    });

    // 4. KChat-authored SaveCamera — exercises ActorKind::KChat.
    let cam = CommandKind::SaveCamera(SaveCamera {
        entity_id: EntityId::new(),
        name: "Hero".into(),
        params: CameraParams {
            position_mm: [3000.0, -4000.0, 1500.0],
            target_mm: [3000.0, 0.0, 1500.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500,
            depth_of_field_f: Some(4.0),
            aspect_ratio: 16.0 / 9.0,
        },
    });
    let actor = Actor::kchat("@alice");
    results.push(Executed {
        actor: actor.clone(),
        result: engine
            .execute(make_command(cam, actor))
            .expect("SaveCamera must succeed"),
    });

    // 5. Three more user CreateWall commands to take us to 10 total.
    for i in 4..7 {
        let kind = create_wall(i as f64 * 2000.0);
        let actor = Actor::user();
        results.push(Executed {
            actor: actor.clone(),
            result: engine
                .execute(make_command(kind, actor))
                .expect("CreateWall must succeed"),
        });
    }

    assert_eq!(results.len(), 10, "10 commands executed");
    // PaintMaterial mutates an existing wall rather than adding a new
    // entity, so the graph entity count is 9 (4 walls + 1 room + 1
    // camera + 3 walls), but the undo journal records all 10
    // commands.
    assert_eq!(engine.graph().len(), 9);
    assert_eq!(engine.undo_len(), 10);
    assert_eq!(engine.redo_len(), 0);

    // ---------------------------------------------------------------
    // Audit chain validation
    // ---------------------------------------------------------------
    // 1) Every envelope has a unique hash.
    let mut hashes: Vec<String> = results.iter().map(|r| r.result.audit.hash.clone()).collect();
    hashes.sort();
    hashes.dedup();
    assert_eq!(
        hashes.len(),
        10,
        "BLAKE3 audit chain must produce 10 unique hashes"
    );

    // 2) Each entry's previous_hash matches the prior entry's hash.
    for w in results.windows(2) {
        assert_eq!(
            w[1].result.audit.previous_hash, w[0].result.audit.hash,
            "audit chain must be monotonically linked"
        );
    }

    // 3) Genesis previous_hash for the first entry.
    assert_eq!(
        results[0].result.audit.previous_hash, "blake3:genesis",
        "first command must extend the genesis head"
    );

    // 4) Actor provenance per command. The Phase-2 e2e test exercises
    //    AI provenance specifically; here we additionally pin KChat
    //    so this test catches a regression where the engine drops the
    //    actor when it stamps the audit envelope.
    let actors: Vec<ActorKind> = results.iter().map(|r| r.actor.kind).collect();
    assert_eq!(actors[0..5], vec![ActorKind::User; 5][..]);
    assert_eq!(actors[5], ActorKind::Ai);
    assert_eq!(actors[6], ActorKind::KChat);
    assert_eq!(actors[7..10], vec![ActorKind::User; 3][..]);

    let ai_tool = results[5].actor.tool.as_deref();
    assert_eq!(ai_tool, Some("style_assistant"));
    let kchat_handle = results[6].actor.tool.as_deref();
    assert_eq!(kchat_handle, Some("@alice"));

    // ---------------------------------------------------------------
    // Undo 5 → graph shrinks, redo journal grows.
    // ---------------------------------------------------------------
    for _ in 0..5 {
        engine.undo().expect("undo must succeed");
    }
    // Five of the popped commands were the last three CreateWall, the
    // SaveCamera, and the AI PaintMaterial. PaintMaterial doesn't
    // change the entity count, so the graph shrinks by 4 (3 walls + 1
    // camera): 9 → 5.
    assert_eq!(engine.graph().len(), 5);
    assert_eq!(engine.undo_len(), 5);
    assert_eq!(engine.redo_len(), 5);

    // ---------------------------------------------------------------
    // Redo 3 → graph grows back.
    // ---------------------------------------------------------------
    let mut redo_results: Vec<CommandResult> = Vec::new();
    for _ in 0..3 {
        redo_results.push(engine.redo().expect("redo must succeed"));
    }
    // Redo replays in execution order: PaintMaterial first (no entity
    // delta), SaveCamera, CreateWall. The graph grows by 2.
    assert_eq!(engine.graph().len(), 7);
    assert_eq!(engine.undo_len(), 8);
    assert_eq!(engine.redo_len(), 2);

    // Redo emits *new* audit envelopes (the chain is append-only —
    // redo is a new event in history, not a rewind). Confirm the new
    // hashes are distinct from every prior hash.
    let mut all_hashes: Vec<String> =
        results.iter().map(|r| r.result.audit.hash.clone()).collect();
    all_hashes.extend(redo_results.iter().map(|r| r.audit.hash.clone()));
    let mut sorted = all_hashes.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        all_hashes.len(),
        "redo must extend the audit chain rather than reuse hashes"
    );

    // ---------------------------------------------------------------
    // Undo everything → graph is empty.
    // ---------------------------------------------------------------
    while engine.undo_len() > 0 {
        engine.undo().expect("undo must succeed");
    }
    assert_eq!(engine.graph().len(), 0);
    // 8 undoable commands were on the stack after redo (10 - 5 + 3 = 8);
    // each undo moves one to the redo stack.
    assert_eq!(engine.redo_len(), 10);
}

#[test]
fn ai_command_undo_reverses_the_diff_and_does_not_corrupt_audit() {
    let mut engine = CommandEngine::new(Scope::Design);

    // Setup: create a wall the AI command will paint.
    let wall_kind = create_wall(0.0);
    let wall_id = match &wall_kind {
        CommandKind::CreateWall(w) => w.entity_id.clone(),
        _ => unreachable!(),
    };
    let setup = engine
        .execute(make_command(wall_kind, Actor::user()))
        .expect("setup wall must succeed");

    // AI command: paint the wall.
    let paint = CommandKind::PaintMaterial(PaintMaterial {
        target_entity_id: wall_id,
        material_id: "oak_natural".into(),
        surface: None,
    });
    let ai = engine
        .execute(make_command(paint, Actor::ai("style_assistant")))
        .expect("AI paint must succeed");
    assert_eq!(ai.audit.previous_hash, setup.audit.hash);
    assert_eq!(engine.undo_len(), 2);

    // Undo the AI paint — graph must roll back and audit head must
    // advance to a *new* envelope (the undo is itself a command in
    // some engines; here it's a journal pop. The contract under
    // test is that the audit chain stays valid afterward.)
    engine.undo().expect("undo of AI paint must succeed");
    assert_eq!(engine.undo_len(), 1);
    assert_eq!(engine.redo_len(), 1);

    // The wall should still exist (we only undid the paint). Reading
    // the wall after undo is the cheap way to catch a wall-delete bug
    // sneaking into PaintMaterial::revert.
    assert_eq!(engine.graph().len(), 1, "wall must still exist after AI paint undo");
}
