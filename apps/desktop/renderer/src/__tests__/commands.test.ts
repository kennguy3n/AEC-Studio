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

  it("paintMaterial issues an Update delta and the body merges", async () => {
    const projectPath = `/projects/paint_${Date.now()}.aecstudio`;
    await createWall(projectPath, {
      entity_id: "wall_paint",
      start_mm: [0, 0],
      end_mm: [3000, 0],
      height_mm: 2700,
      thickness_mm: 100,
      material_id: "white",
    });
    const result = await paintMaterial(projectPath, {
      entity_id: "wall_paint",
      material_id: "oak",
    });
    expect(result.applied).toHaveLength(1);
    const delta = result.applied[0]!;
    expect(delta.kind).toBe("update");
    if (delta.kind !== "update") throw new Error("expected update delta");
    expect((delta.after as { material_id: string }).material_id).toBe("oak");
    // Body merge — existing fields preserved.
    expect((delta.after as { height_mm: number }).height_mm).toBe(2700);
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

  it("saveCamera persists camera entities under kind=camera", async () => {
    const projectPath = `/projects/cam_${Date.now()}.aecstudio`;
    await saveCamera(projectPath, {
      entity_id: "cam_1",
      name: "Hero",
      position_mm: [1000, 1000, 1500],
      target_mm: [0, 0, 1500],
      fov_deg: 45,
    });
    const rows = await projectGraphList(projectPath, "camera");
    expect(rows).toHaveLength(1);
    expect(rows[0]!.id).toBe("cam_1");
    expect((rows[0]!.body as { name: string }).name).toBe("Hero");
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
      position_mm: [0, 0, 0],
      target_mm: [1, 0, 0],
      fov_deg: 60,
    });
    const all = await projectGraphList(projectPath);
    expect(all).toHaveLength(2);
    expect(new Set(all.map((r) => r.kind))).toEqual(new Set(["wall", "camera"]));
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
});
