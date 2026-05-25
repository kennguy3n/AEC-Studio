import { describe, expect, it } from "vitest";

import { inProcessBackend } from "../../../electron/bridge";

/**
 * The in-process backend's draft.* and deliver.* methods (Group A,
 * Phase 10) pin the renderer-facing contract independently of the
 * native bridge. The catalogue test enforces wired/fallback
 * disjointness; these tests pin the exact shape every caller
 * receives.
 */
describe("draft.* in-process contract", () => {
  it("draftDrawPrimitive returns a stable { entityId } shape", async () => {
    const b = inProcessBackend();
    const r = await b.draftDrawPrimitive({
      projectPath: "/projects/x.aecstudio",
      type: "line",
      start: [0, 0],
      end: [10, 0],
    });
    expect(typeof r.entityId).toBe("string");
    expect(r.entityId.length).toBeGreaterThan(0);
  });

  it("draftEditTool returns { ok: true }", async () => {
    const b = inProcessBackend();
    const r = await b.draftEditTool({
      projectPath: "/projects/x.aecstudio",
      tool: "move",
      targets: ["a", "b"],
      delta: [5, 0],
    });
    expect(r).toEqual({ ok: true });
  });

  it("draftCreateSheet returns a sheetId", async () => {
    const b = inProcessBackend();
    const r = await b.draftCreateSheet({
      projectPath: "/projects/x.aecstudio",
      name: "A101",
      paperSize: "A1",
      scale: "1:100",
    });
    expect(typeof r.sheetId).toBe("string");
  });

  it("draftSetLayerState returns { ok: true }", async () => {
    const b = inProcessBackend();
    const r = await b.draftSetLayerState({
      projectPath: "/projects/x.aecstudio",
      name: "WALLS",
      color: 7,
      frozen: false,
    });
    expect(r).toEqual({ ok: true });
  });

  it("draftImportDxf returns { imported: number }", async () => {
    const b = inProcessBackend();
    const r = await b.draftImportDxf({
      projectPath: "/projects/x.aecstudio",
      dxfPath: "/tmp/in.dxf",
    });
    expect(typeof r.imported).toBe("number");
  });

  it("draftExportDxf echoes back the dxfPath", async () => {
    const b = inProcessBackend();
    const r = await b.draftExportDxf({
      projectPath: "/projects/x.aecstudio",
      dxfPath: "/tmp/out.dxf",
    });
    expect(r.exported).toBe(true);
    expect(r.path).toBe("/tmp/out.dxf");
  });

  it("draftImportDwg returns { imported: number, version: string }", async () => {
    const b = inProcessBackend();
    const r = await b.draftImportDwg({
      projectPath: "/projects/x.aecstudio",
      dwgPath: "/tmp/in.dwg",
    });
    expect(typeof r.imported).toBe("number");
    // The fallback uses `AC0000` as a sentinel so callers can tell the
    // headless / no-native-artefact path apart from a real native
    // import (which always reports a real `AC10xx` tag).
    expect(r.version).toBe("AC0000");
  });

  it("draftExportDwg echoes back the dwgPath and the requested version", async () => {
    const b = inProcessBackend();
    const r = await b.draftExportDwg({
      projectPath: "/projects/x.aecstudio",
      dwgPath: "/tmp/out.dwg",
      version: "AC1018",
    });
    expect(r.exported).toBe(true);
    expect(r.path).toBe("/tmp/out.dwg");
    expect(r.version).toBe("AC1018");
  });

  it("draftExportDwg defaults to R2004 (AC1018) when version is omitted", async () => {
    const b = inProcessBackend();
    const r = await b.draftExportDwg({
      projectPath: "/projects/x.aecstudio",
      dwgPath: "/tmp/out-default.dwg",
    });
    expect(r.version).toBe("AC1018");
  });
});

describe("deliver.* in-process contract", () => {
  it("creates a tagged revision and lists it back", async () => {
    const b = inProcessBackend();
    const rev = await b.deliverCreateRevision({
      projectPath: "/projects/x.aecstudio",
      tag: "v1",
      description: "first cut",
      entities: [
        {
          category: "wall",
          id: "ent_1",
          payloadHash: "h1",
          label: "South Wall",
        },
      ],
    });
    expect(rev.tag).toBe("v1");
    expect(rev.description).toBe("first cut");
    expect(rev.trackedEntities).toHaveLength(1);
    expect(rev.trackedEntities[0].label).toBe("South Wall");
    expect(typeof rev.revisionId).toBe("string");

    const list = await b.deliverListRevisions({
      projectPath: "/projects/x.aecstudio",
    });
    expect(list.some((r) => r.revisionId === rev.revisionId)).toBe(true);
  });

  it("rejects duplicate tags", async () => {
    const b = inProcessBackend();
    await b.deliverCreateRevision({
      projectPath: "/projects/x.aecstudio",
      tag: "v-dup",
      description: "first",
    });
    await expect(
      b.deliverCreateRevision({
        projectPath: "/projects/x.aecstudio",
        tag: "v-dup",
        description: "second",
      }),
    ).rejects.toThrow(/already exists/);
  });

  it("rejects missing projectPath on all three deliver methods", async () => {
    const b = inProcessBackend();
    await expect(
      // @ts-expect-error — intentionally omitting projectPath
      b.deliverCreateRevision({ tag: "x", description: "y" }),
    ).rejects.toThrow(/projectPath/);
    await expect(
      // @ts-expect-error — intentionally omitting projectPath
      b.deliverListRevisions({}),
    ).rejects.toThrow(/projectPath/);
    await expect(
      // @ts-expect-error — intentionally omitting projectPath
      b.deliverCompareRevisions({ baseId: "a", headId: "b" }),
    ).rejects.toThrow(/projectPath/);
  });

  it("diffs two revisions and reports per-category counts", async () => {
    const b = inProcessBackend();
    const a = await b.deliverCreateRevision({
      projectPath: "/projects/x.aecstudio",
      tag: "diff-a",
      description: "base",
      entities: [
        { category: "wall", id: "w1", payloadHash: "h1" },
      ],
    });
    const c = await b.deliverCreateRevision({
      projectPath: "/projects/x.aecstudio",
      tag: "diff-b",
      description: "head",
      entities: [
        // w1 modified (different hash)
        { category: "wall", id: "w1", payloadHash: "h2" },
        // w2 added
        { category: "wall", id: "w2", payloadHash: "h3" },
      ],
    });
    const diff = await b.deliverCompareRevisions({
      projectPath: "/projects/x.aecstudio",
      baseId: a.revisionId,
      headId: c.revisionId,
    });
    expect(diff.byCategory.wall).toBeDefined();
    expect(diff.byCategory.wall.added).toBe(1);
    expect(diff.byCategory.wall.modified).toBe(1);
    // Each kind shows up on the per-entity changes list.
    const w2 = diff.changes.find((ch) => ch.id === "w2");
    expect(w2?.kind).toBe("added");
    const w1 = diff.changes.find((ch) => ch.id === "w1");
    expect(w1?.kind).toBe("modified");
  });
});
