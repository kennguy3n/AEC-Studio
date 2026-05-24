import { describe, it, expect, vi, beforeEach } from "vitest";
import type { BimAttachSummary } from "../../../electron/bridge";
import { attachIfcToProject } from "../api/bim-attach";
import { aec } from "../api/aec";

// Helper: build a populated `BimAttachSummary` for the mock. The
// real bridge returns post-dedup counts (`_inserted` / `_updated` /
// `_unchanged`) for spatial nodes and elements; this helper lets
// individual tests assert on the counts they care about without
// restating every zero-valued field.
const sum = (over: Partial<BimAttachSummary> = {}): BimAttachSummary => ({
  path: "/abs/file.ifc",
  projectPath: "/abs/project.aecstudio",
  parseCacheHit: false,
  spatialNodesInserted: 0,
  spatialNodesUpdated: 0,
  spatialNodesUnchanged: 0,
  elementsInserted: 0,
  elementsUpdated: 0,
  elementsUnchanged: 0,
  componentsInserted: 0,
  relationsInserted: 0,
  cacheRows: 0,
  ...over,
});

// The attach contract is symmetric with `importIfcWithSizeGuard`:
//   * On success the renderer receives a tagged `"attached"` outcome
//     carrying the full `BimAttachSummary`.
//   * On bridge error the renderer receives a `"failed"` outcome
//     carrying the error message — NEVER a thrown exception, so
//     the React error boundary stays clean.
//
// These tests pin both paths, the parse-cache-hit reporting that
// signals an import→attach handoff served the parse for free, and
// the dedup contract (unchanged re-attach reports `_unchanged` ≠ 0).

describe("attachIfcToProject", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns attached outcome on a successful first attach", async () => {
    const attachSpy = vi
      .spyOn(aec.bim, "attachIfc")
      .mockResolvedValue(
        sum({
          path: "/abs/test.ifc",
          projectPath: "/abs/project.aecstudio",
          spatialNodesInserted: 4,
          elementsInserted: 87,
          componentsInserted: 174,
          relationsInserted: 91,
          cacheRows: 87,
        }),
      );

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/abs/test.ifc",
    );

    expect(outcome.kind).toBe("attached");
    if (outcome.kind === "attached") {
      expect(outcome.result.path).toBe("/abs/test.ifc");
      expect(outcome.result.projectPath).toBe("/abs/project.aecstudio");
      expect(outcome.result.spatialNodesInserted).toBe(4);
      expect(outcome.result.elementsInserted).toBe(87);
      expect(outcome.result.componentsInserted).toBe(174);
    }
    expect(attachSpy).toHaveBeenCalledOnce();
    expect(attachSpy).toHaveBeenCalledWith(
      "/abs/project.aecstudio",
      "/abs/test.ifc",
    );
  });

  it("reports parseCacheHit when a prior importIfc served the parse", async () => {
    // Simulates the import → attach handoff: the renderer just ran
    // `importIfcWithSizeGuard("/abs/test.ifc")` and the snapshot
    // cache is warm. The native bridge reports cacheHit so the
    // renderer's loading UI can skip the "parsing..." spinner.
    vi.spyOn(aec.bim, "attachIfc").mockResolvedValue(
      sum({
        path: "/abs/test.ifc",
        projectPath: "/abs/project.aecstudio",
        parseCacheHit: true,
        spatialNodesInserted: 4,
        elementsInserted: 87,
      }),
    );

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/abs/test.ifc",
    );

    expect(outcome.kind).toBe("attached");
    if (outcome.kind === "attached") {
      expect(outcome.result.parseCacheHit).toBe(true);
    }
  });

  it("reports unchanged counts on a redundant re-attach (dedup contract)", async () => {
    // The bim_attach service deduplicates by content fingerprint:
    // a re-attach of the same file with no changes increments the
    // _unchanged counters, NOT _inserted / _updated. The renderer
    // displays the triplet so the user can tell "0 inserted, 0
    // updated, 87 unchanged" apart from "this didn't run".
    vi.spyOn(aec.bim, "attachIfc").mockResolvedValue(
      sum({
        path: "/abs/test.ifc",
        projectPath: "/abs/project.aecstudio",
        parseCacheHit: true,
        spatialNodesUnchanged: 4,
        elementsUnchanged: 87,
      }),
    );

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/abs/test.ifc",
    );

    expect(outcome.kind).toBe("attached");
    if (outcome.kind === "attached") {
      expect(outcome.result.spatialNodesUnchanged).toBe(4);
      expect(outcome.result.elementsUnchanged).toBe(87);
      expect(outcome.result.spatialNodesInserted).toBe(0);
      expect(outcome.result.elementsInserted).toBe(0);
    }
  });

  it("returns failed outcome (no throw) when bridge errors", async () => {
    vi.spyOn(aec.bim, "attachIfc").mockRejectedValue(
      new Error("project database is locked"),
    );

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/abs/test.ifc",
    );

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.error).toContain("project database is locked");
    }
  });

  it("surfaces string errors verbatim", async () => {
    // The bridge layer normally wraps errors as `Error` instances,
    // but defensively the renderer accepts plain-string rejections
    // (e.g. an early failure in the IPC marshalling).
    vi.spyOn(aec.bim, "attachIfc").mockRejectedValue(
      "ENOENT: no such file",
    );

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/missing.ifc",
    );

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.error).toBe("ENOENT: no such file");
    }
  });

  it("does not throw on non-Error, non-string rejections", async () => {
    // Defensive: if something exotic bubbles up (e.g. an N-API
    // panic surfaced as a plain object), the renderer should
    // still surface a `"failed"` outcome rather than re-throw.
    vi.spyOn(aec.bim, "attachIfc").mockRejectedValue({ code: 42 });

    const outcome = await attachIfcToProject(
      "/abs/project.aecstudio",
      "/x.ifc",
    );

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.error).toBe('{"code":42}');
    }
  });

  it("passes projectPath and ifcPath through verbatim (no canonicalisation here)", async () => {
    // Canonicalisation is done bridge-side (Rust `fs::canonicalize`).
    // The renderer must NOT independently canonicalise — that would
    // create a drift window where the import-path and attach-path
    // disagree, busting the snapshot-cache key.
    const attachSpy = vi.spyOn(aec.bim, "attachIfc").mockResolvedValue(
      sum({
        path: "/canonical/test.ifc",
        projectPath: "/canonical/project.aecstudio",
      }),
    );

    await attachIfcToProject(
      "./relative-project.aecstudio",
      "../uncanonical/./test.ifc",
    );

    expect(attachSpy).toHaveBeenCalledWith(
      "./relative-project.aecstudio",
      "../uncanonical/./test.ifc",
    );
  });
});
