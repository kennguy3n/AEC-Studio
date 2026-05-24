import { describe, it, expect } from "vitest";
import {
  buildUserCommand,
  buildAiCommand,
  commandApply,
  commandRedo,
  commandUndo,
  createWall,
  deleteWall,
  newCommandId,
  paintMaterial,
  projectGraphList,
  saveCamera,
  setLighting,
} from "../api/commands";

/**
 * Vitest exercises the renderer-side command surface against the
 * in-process fallback (`renderer-backend.ts`'s `commandMock`), which
 * mirrors the Rust `aec_command::engine::CommandEngine` semantics
 * 1:1 (forward delta → apply → push to undo, undo → pop → apply
 * inverse → push to redo, new apply → clear redo).
 *
 * The end-to-end native path is pinned separately by
 * `crates/aec_bridge::service::tests::command_*` (Rust integration)
 * + `cargo test -p aec_command` (engine + journal + graph unit tests).
 */
describe("renderer command surface", () => {
  it("mints command ids with the expected `cmd_` prefix", () => {
    const id = newCommandId();
    expect(id).toMatch(/^cmd_[0-9a-f]{32}$/);
    // Two consecutive mints should not collide.
    expect(newCommandId()).not.toBe(id);
  });

  it("buildUserCommand attaches scope / actor / ts / fresh id", () => {
    const cmd = buildUserCommand("design", "design.create_wall", { foo: 1 });
    expect(cmd.scope).toBe("design");
    expect(cmd.tool).toBe("design.create_wall");
    expect(cmd.actor).toEqual({ kind: "user" });
    expect(cmd.command_id).toMatch(/^cmd_/);
    expect(typeof cmd.ts).toBe("string");
    expect(new Date(cmd.ts).toString()).not.toBe("Invalid Date");
  });

  it("buildAiCommand carries the originating AI tool in the actor", () => {
    const cmd = buildAiCommand(
      "design",
      "layout_suggestion",
      "design.create_room",
      { polygon_mm: [] },
    );
    expect(cmd.actor).toEqual({ kind: "ai", tool: "layout_suggestion" });
  });

  it("createWall applies a Create delta with kind=wall and updates the graph", async () => {
    const projectPath = `/projects/create_${Date.now()}.aecstudio`;
    const before = await projectGraphList(projectPath, "wall");
    expect(before).toEqual([]);

    const result = await createWall(projectPath, {
      start_mm: [0, 0],
      end_mm: [4500, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    expect(result.applied).toHaveLength(1);
    const delta = result.applied[0]!;
    expect(delta.kind).toBe("create");
    if (delta.kind !== "create") throw new Error("expected create delta");
    expect(delta.record.kind).toBe("wall");
    expect(result.undoLen).toBe(1);
    expect(result.redoLen).toBe(0);

    const after = await projectGraphList(projectPath, "wall");
    expect(after).toHaveLength(1);
    expect(after[0]!.kind).toBe("wall");
  });

  it("commandUndo reverses a create and commandRedo re-applies it", async () => {
    const projectPath = `/projects/undo_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_001",
      start_mm: [0, 0],
      end_mm: [4500, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });

    const undone = await commandUndo(projectPath, "design");
    expect(undone.applied).toHaveLength(1);
    expect(undone.applied[0]!.kind).toBe("delete");
    expect(undone.undoLen).toBe(0);
    expect(undone.redoLen).toBe(1);
    expect(await projectGraphList(projectPath, "wall")).toEqual([]);

    const redone = await commandRedo(projectPath, "design");
    expect(redone.applied).toHaveLength(1);
    expect(redone.applied[0]!.kind).toBe("create");
    expect(redone.undoLen).toBe(1);
    expect(redone.redoLen).toBe(0);
    expect(await projectGraphList(projectPath, "wall")).toHaveLength(1);
  });

  it("a fresh apply clears the redo stack", async () => {
    const projectPath = `/projects/redo_clear_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_first",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    await commandUndo(projectPath, "design");
    const reapplied = await createWall(projectPath, {
      entity_id: "wall_second",
      start_mm: [0, 0],
      end_mm: [3500, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    expect(reapplied.undoLen).toBe(1);
    // Per Rust engine semantics, any fresh apply drops the redo
    // stack — there's no "branch the timeline" mode.
    expect(reapplied.redoLen).toBe(0);
  });

  it("paintMaterial issues a surgical Update delta against target_entity_id", async () => {
    const projectPath = `/projects/paint_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_paint",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
      material_id: "white",
    });
    // Field name is `target_entity_id` (not `entity_id`) to match the
    // Rust `aec_command::commands::material::PaintMaterial` struct.
    const result = await paintMaterial(projectPath, {
      target_entity_id: "wall_paint",
      material_id: "oak",
    });
    expect(result.applied).toHaveLength(1);
    const delta = result.applied[0]!;
    expect(delta.kind).toBe("update");
    if (delta.kind !== "update") throw new Error("expected update delta");
    const after = delta.after as Record<string, unknown>;
    // `material_id` was overwritten; other body fields preserved.
    expect(after.material_id).toBe("oak");
    expect(after.height_mm).toBe(2700);
    // Rust's `PaintMaterial::to_delta` only inserts `material_id`
    // (or the surface slot) into the *existing* wall body; it never
    // touches the routing-only `target_entity_id` field. The wall's
    // serialised body comes from `CreateWall::to_delta`'s
    // `serde_json::to_value(self)`, which carries `entity_id` (the
    // wall's own id) but never carries a `target_entity_id` field
    // because `CreateWall` doesn't have one. So the routing field
    // must remain absent here.
    expect(after.target_entity_id).toBeUndefined();
  });

  it("paintMaterial with `surface` writes into surface_materials instead of overwriting material_id", async () => {
    const projectPath = `/projects/paint_surface_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_surface",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
      material_id: "white",
    });
    const result = await paintMaterial(projectPath, {
      target_entity_id: "wall_surface",
      material_id: "oak",
      surface: "wall:interior",
    });
    const delta = result.applied[0]!;
    if (delta.kind !== "update") throw new Error("expected update delta");
    const after = delta.after as Record<string, unknown>;
    // Whole-entity material is untouched.
    expect(after.material_id).toBe("white");
    // Surface map carries the new per-surface assignment.
    expect(after.surface_materials).toEqual({ "wall:interior": "oak" });
  });

  it("deleteWall removes the entity from the graph", async () => {
    const projectPath = `/projects/delete_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_del",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    expect(await projectGraphList(projectPath, "wall")).toHaveLength(1);
    const result = await deleteWall(projectPath, "wall_del");
    expect(result.applied[0]!.kind).toBe("delete");
    expect(await projectGraphList(projectPath, "wall")).toEqual([]);
  });

  it("saveCamera persists camera entities under kind=camera with nested params", async () => {
    const projectPath = `/projects/cam_${Date.now()}.aecstudio`;
    // Shape mirrors `aec_command::commands::camera::SaveCamera` 1:1:
    // `params` is nested under `CameraParams`, and the field is
    // `focal_length_mm` (not the legacy `fov_deg`).
    await saveCamera(projectPath, {
      entity_id: "cam_1",
      name: "Hero",
      params: {
        position_mm: [1000, 1000, 1500],
        target_mm: [0, 0, 1500],
        focal_length_mm: 35,
        exposure_ev: 0,
        white_balance_k: 5500,
        depth_of_field_f: 2.8,
        aspect_ratio: 16 / 9,
      },
    });
    const rows = await projectGraphList(projectPath, "camera");
    expect(rows).toHaveLength(1);
    expect(rows[0]!.id).toBe("cam_1");
    const body = rows[0]!.body as { name: string; params: { focal_length_mm: number } };
    expect(body.name).toBe("Hero");
    expect(body.params.focal_length_mm).toBe(35);
    // `SaveCamera::to_delta` writes `serde_json::to_value(self)` as
    // the body, which includes the `entity_id` field. The native
    // path therefore round-trips with `entity_id` present in the
    // body; the in-process fallback matches this contract via
    // `createBodyFor("design.save_camera", args)` in
    // `apps/desktop/electron/bridge.ts`. Pin both backends to the
    // same shape.
    expect((rows[0]!.body as Record<string, unknown>).entity_id).toBe("cam_1");
  });

  it("projectGraphList with no kindFilter returns the full graph", async () => {
    const projectPath = `/projects/all_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "w_all_1",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    await saveCamera(projectPath, {
      entity_id: "c_all_1",
      name: "All",
      params: {
        position_mm: [0, 0, 0],
        target_mm: [1, 0, 0],
        focal_length_mm: 50,
        exposure_ev: 0,
        white_balance_k: 5500,
        aspect_ratio: 16 / 9,
      },
    });
    const all = await projectGraphList(projectPath);
    expect(all).toHaveLength(2);
    expect(new Set(all.map((r) => r.kind))).toEqual(new Set(["wall", "camera"]));
    // Root entities must have `parent === null`, never `undefined` —
    // pins the BUG_0001 normalisation contract from PR-Q round 3.
    for (const r of all) {
      expect(r.parent).toBeNull();
    }
  });

  it("commandApply on the same projectPath shares state across calls", async () => {
    const projectPath = `/projects/shared_${Date.now()}.aecstudio`;
    const created = await commandApply(
      projectPath,
      buildUserCommand("design", "design.create_wall", {
        entity_id: "wall_shared",
        start_mm: [0, 0],
        end_mm: [3000, 0],
        height_mm: 2700,
        thickness_mm: 100,
      }),
    );
    expect(created.undoLen).toBe(1);

    const undone = await commandApply(
      projectPath,
      buildUserCommand("design", "design.delete_wall", {
        entity_id: "wall_shared",
      }),
    );
    expect(undone.applied[0]!.kind).toBe("delete");
    expect(undone.undoLen).toBe(2);
  });

  it("undo on an empty journal rejects with a clear error", async () => {
    const projectPath = `/projects/empty_${Date.now()}.aecstudio`;
    await expect(commandUndo(projectPath, "design")).rejects.toThrow(/nothing to undo/i);
  });

  // Pins scope validation on undo/redo. The renderer-facing contract
  // (BridgeBackend.commandUndo doc comment) and the native engine
  // both reject undoing a command tagged with one scope while the
  // active scope is another. The in-process fallback must enforce
  // the same invariant or dev/test mode would silently accept
  // mismatched undo calls that production would reject.
  it("commandUndo rejects when activeScope disagrees with the journal entry's scope", async () => {
    const projectPath = `/projects/scope_undo_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_scope",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    // The wall was created under `design`; an undo issued from
    // any other scope must be rejected before either stack moves.
    await expect(commandUndo(projectPath, "bim")).rejects.toThrow(/scope mismatch/i);
    // Journal is untouched: a follow-up undo from the correct
    // scope still succeeds.
    const undone = await commandUndo(projectPath, "design");
    expect(undone.applied).toHaveLength(1);
    expect(undone.undoLen).toBe(0);
    expect(undone.redoLen).toBe(1);
  });

  it("commandRedo rejects when activeScope disagrees with the journal entry's scope", async () => {
    const projectPath = `/projects/scope_redo_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_redo_scope",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    await commandUndo(projectPath, "design");
    await expect(commandRedo(projectPath, "render")).rejects.toThrow(/scope mismatch/i);
    const redone = await commandRedo(projectPath, "design");
    expect(redone.applied[0]!.kind).toBe("create");
    expect(redone.undoLen).toBe(1);
  });

  // Pins the audit-only contract for `design.set_lighting`. The Rust
  // engine returns zero `EntityDelta`s for `SetLighting`, so the
  // in-process fallback must do the same — otherwise undoing a
  // lighting change would silently revert an entity body in dev/test
  // mode but be a no-op on the native backend.
  it("setLighting produces zero deltas (audit-only) but advances the undo journal", async () => {
    const projectPath = `/projects/set_lighting_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_lit",
      start_mm: [0, 0],
      end_mm: [4500, 0],
      height_mm: 2700,
      thickness_mm: 100,
    });
    const before = await projectGraphList(projectPath, "wall");
    expect(before).toHaveLength(1);
    const wallBodyBefore = before[0]!.body;

    const result = await setLighting(projectPath, { preset_id: "warm_evening" });
    expect(result.applied).toEqual([]);
    expect(result.undoLen).toBe(2); // create_wall + set_lighting both recorded
    expect(result.redoLen).toBe(0);

    // The wall body must be unchanged — set_lighting is audit-only.
    const afterApply = await projectGraphList(projectPath, "wall");
    expect(afterApply).toHaveLength(1);
    expect(afterApply[0]!.body).toEqual(wallBodyBefore);

    // Undoing set_lighting must not touch the graph either.
    const undone = await commandUndo(projectPath, "design");
    expect(undone.applied).toEqual([]);
    expect(undone.undoLen).toBe(1);
    expect(undone.redoLen).toBe(1);
    const afterUndo = await projectGraphList(projectPath, "wall");
    expect(afterUndo).toHaveLength(1);
    expect(afterUndo[0]!.body).toEqual(wallBodyBefore);
  });

  it("setLighting rejects empty preset_id", async () => {
    const projectPath = `/projects/set_lighting_invalid_${Date.now()}.aecstudio`;
    await expect(setLighting(projectPath, { preset_id: "" })).rejects.toThrow(
      /preset_id must not be empty/i,
    );
    await expect(setLighting(projectPath, { preset_id: "   " })).rejects.toThrow(
      /preset_id must not be empty/i,
    );
  });
});
