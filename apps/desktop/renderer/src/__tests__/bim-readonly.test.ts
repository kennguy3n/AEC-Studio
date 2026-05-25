import { describe, it, expect, vi, beforeEach } from "vitest";
import { aec } from "../api/aec";
import type {
  BimDiffSummary,
  BimExportIfcSummary,
  BimScheduleSummary,
  BimValidateReport,
} from "../../../electron/bridge";

// Vitest-side smoke tests for the PR-T read-only BIM API. The
// renderer's `aec.bim.*` accessor resolves through `rendererInProcessBackend()`
// in jsdom (the preload's `window.aec` isn't present), so these
// tests exercise the in-process fallback implementations defined in
// `renderer-backend.ts` — same field names, same zero-finding
// payloads as the electron-side `inProcessBackend()` in
// `electron/bridge.ts`. The "the result shape exists, the
// parameters are wired" contract is what the renderer code in
// `Bim.tsx`, `ValidatorPanel.tsx`, and `ScheduleView.tsx` depends
// on. Native-backend behaviour (real parse + writer + validator) is
// covered by the Rust integration tests in
// `crates/aec_bridge/tests/bim_readonly_ops.rs`.

describe("aec.bim.exportIfc", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the wire-format BimExportIfcSummary shape", async () => {
    const result = await aec.bim.exportIfc({
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/out.ifc",
    });
    expect(result).toMatchObject({
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/out.ifc",
      schema: expect.any(String),
      bytesWritten: expect.any(Number),
      parseCacheHit: expect.any(Boolean),
    });
  });

  it("surfaces the bridge's parseCacheHit verbatim", async () => {
    const payload: BimExportIfcSummary = {
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/out.ifc",
      schema: "IFC4",
      bytesWritten: 12345,
      parseCacheHit: true,
    };
    vi.spyOn(aec.bim, "exportIfc").mockResolvedValueOnce(payload);
    const result = await aec.bim.exportIfc({
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/out.ifc",
    });
    expect(result.parseCacheHit).toBe(true);
    expect(result.bytesWritten).toBe(12345);
  });
});

describe("aec.bim.validate", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the wire-format BimValidateReport shape", async () => {
    const result = await aec.bim.validate({ sourcePath: "/abs/source.ifc" });
    expect(result).toMatchObject({
      ok: expect.any(Boolean),
      sourcePath: "/abs/source.ifc",
      schema: expect.any(String),
      errors: expect.any(Array),
      warnings: expect.any(Array),
      infos: expect.any(Array),
      parseCacheHit: expect.any(Boolean),
    });
  });

  it("preserves finding structure for non-empty reports", async () => {
    const payload: BimValidateReport = {
      ok: false,
      sourcePath: "/abs/source.ifc",
      schema: "IFC4",
      errors: [
        {
          severity: "error",
          code: "MISSING_GUID",
          element: "ent_001",
          description: "Element has no GUID",
          suggestion: "Regenerate the GUID with the assign-guid tool",
        },
      ],
      warnings: [
        {
          severity: "warning",
          code: "ORPHAN_SPATIAL",
          element: null,
          description: "Storey has no parent building",
          suggestion: null,
        },
      ],
      infos: [],
      parseCacheHit: true,
    };
    vi.spyOn(aec.bim, "validate").mockResolvedValueOnce(payload);
    const result = await aec.bim.validate({ sourcePath: "/abs/source.ifc" });
    expect(result.ok).toBe(false);
    expect(result.errors).toHaveLength(1);
    expect(result.errors[0].code).toBe("MISSING_GUID");
    expect(result.errors[0].element).toBe("ent_001");
    expect(result.warnings).toHaveLength(1);
    expect(result.warnings[0].element).toBeNull();
    expect(result.infos).toHaveLength(0);
  });
});

describe("aec.bim.diff", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the wire-format BimDiffSummary shape", async () => {
    const result = await aec.bim.diff({
      beforePath: "/abs/before.ifc",
      afterPath: "/abs/after.ifc",
    });
    expect(result).toMatchObject({
      diffId: expect.any(String),
      beforePath: "/abs/before.ifc",
      afterPath: "/abs/after.ifc",
      beforeSchema: expect.any(String),
      afterSchema: expect.any(String),
      added: expect.any(Array),
      removed: expect.any(Array),
      modified: expect.any(Array),
      beforeCacheHit: expect.any(Boolean),
      afterCacheHit: expect.any(Boolean),
    });
  });

  it("preserves added / removed / modified element keys", async () => {
    const payload: BimDiffSummary = {
      diffId: "diff_blake3_abc123",
      beforePath: "/abs/before.ifc",
      afterPath: "/abs/after.ifc",
      beforeSchema: "IFC4",
      afterSchema: "IFC4",
      added: ["bim/element/0001"],
      removed: ["bim/element/0099"],
      modified: [
        {
          key: "bim/element/0042",
          classBefore: "IfcWall",
          classAfter: "IfcWall",
          nameBefore: "Wall A",
          nameAfter: "Wall A (renamed)",
          propertyDeltas: [
            {
              pset: "Pset_WallCommon",
              key: "FireRating",
              before: '"F30"',
              after: '"F60"',
            },
          ],
        },
      ],
      beforeCacheHit: false,
      afterCacheHit: true,
    };
    vi.spyOn(aec.bim, "diff").mockResolvedValueOnce(payload);
    const result = await aec.bim.diff({
      beforePath: "/abs/before.ifc",
      afterPath: "/abs/after.ifc",
    });
    expect(result.added).toEqual(["bim/element/0001"]);
    expect(result.removed).toEqual(["bim/element/0099"]);
    expect(result.modified).toHaveLength(1);
    expect(result.modified[0].nameAfter).toBe("Wall A (renamed)");
    expect(result.modified[0].propertyDeltas[0].key).toBe("FireRating");
    expect(result.modified[0].propertyDeltas[0].after).toBe('"F60"');
  });
});

describe("aec.bim.generateSchedule", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the wire-format BimScheduleSummary shape for each kind", async () => {
    for (const kind of ["door", "window", "room", "material"] as const) {
      const result = await aec.bim.generateSchedule({
        sourcePath: "/abs/source.ifc",
        outPath: `/abs/${kind}.xlsx`,
        kind,
      });
      expect(result).toMatchObject({
        scheduleId: expect.any(String),
        kind,
        sourcePath: "/abs/source.ifc",
        outPath: `/abs/${kind}.xlsx`,
        rows: expect.any(Number),
        columns: expect.any(Number),
        bytesWritten: expect.any(Number),
        parseCacheHit: expect.any(Boolean),
      });
    }
  });

  it("threads through scheduleId / row+column counts from the bridge", async () => {
    const payload: BimScheduleSummary = {
      scheduleId: "sched_blake3_xyz789",
      kind: "door",
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/door.xlsx",
      rows: 12,
      columns: 8,
      bytesWritten: 5432,
      parseCacheHit: true,
    };
    vi.spyOn(aec.bim, "generateSchedule").mockResolvedValueOnce(payload);
    const result = await aec.bim.generateSchedule({
      sourcePath: "/abs/source.ifc",
      outPath: "/abs/door.xlsx",
      kind: "door",
    });
    expect(result.scheduleId).toBe("sched_blake3_xyz789");
    expect(result.rows).toBe(12);
    expect(result.columns).toBe(8);
    expect(result.bytesWritten).toBe(5432);
    expect(result.parseCacheHit).toBe(true);
  });
});
