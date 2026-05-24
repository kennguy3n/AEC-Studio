import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  importIfcWithSizeGuard,
  defaultLargeFileConfirm,
} from "../api/bim-import";
import type { BimImportSummary } from "../../../electron/bridge";
import { aec } from "../api/aec";

// Helper: build a populated `BimImportSummary` shape for the mock.
// Overrides let individual tests assert on the counts they care about
// without restating every zero-valued field.
const summary = (over: Partial<BimImportSummary> = {}): BimImportSummary => ({
  path: "/abs/file.ifc",
  schema: "IFC4",
  spatialNodes: 0,
  elements: 0,
  psets: 0,
  qsets: 0,
  aggregations: 0,
  containments: 0,
  materials: 0,
  materialLayerSets: 0,
  materialAssignments: 0,
  recordsSeen: 0,
  ...over,
});

// The size-guard contract: before invoking the multi-second
// `bim_import_ifc` parse, the renderer issues a cheap
// `bim_check_file_size` stat and — only if the file is at or
// above the warn threshold — pops a confirm dialog. The guard
// must:
//
//   * Skip the dialog entirely for small files (cheap path).
//   * Skip `bim_import_ifc` entirely when the user cancels.
//   * Surface bridge errors as `kind === "failed"` (no throw).
//
// These tests pin all three.

describe("importIfcWithSizeGuard", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("imports without confirm prompt when file is small", async () => {
    const checkSpy = vi.spyOn(aec.bim, "checkFileSize").mockResolvedValue({
      path: "/abs/small.ifc",
      fileSizeBytes: 10 * 1024 * 1024, // 10 MB — well below threshold
      largeFileWarning: false,
      thresholdBytes: 100 * 1024 * 1024,
    });
    const importSpy = vi
      .spyOn(aec.bim, "importIfc")
      .mockResolvedValue(summary({ path: "/abs/small.ifc", spatialNodes: 2, elements: 42 }));
    const confirmStub = vi.fn(() => true);

    const outcome = await importIfcWithSizeGuard(
      "/abs/small.ifc",
      confirmStub,
    );

    expect(outcome.kind).toBe("imported");
    if (outcome.kind === "imported") {
      expect(outcome.result.path).toBe("/abs/small.ifc");
      expect(outcome.result.elements).toBe(42);
    }
    expect(checkSpy).toHaveBeenCalledOnce();
    expect(checkSpy).toHaveBeenCalledWith("/abs/small.ifc");
    expect(importSpy).toHaveBeenCalledOnce();
    expect(importSpy).toHaveBeenCalledWith("/abs/small.ifc");
    expect(
      confirmStub,
      "small files must not invoke the confirm callback",
    ).not.toHaveBeenCalled();
  });

  it("invokes confirm callback when large_file_warning is true, then imports on accept", async () => {
    const checkSpy = vi.spyOn(aec.bim, "checkFileSize").mockResolvedValue({
      path: "/abs/big.ifc",
      fileSizeBytes: 412 * 1024 * 1024, // 412 MB — above threshold
      largeFileWarning: true,
      thresholdBytes: 100 * 1024 * 1024,
    });
    const importSpy = vi
      .spyOn(aec.bim, "importIfc")
      .mockResolvedValue(summary({ path: "/abs/big.ifc", elements: 9999 }));
    const confirmStub = vi.fn(() => true);

    const outcome = await importIfcWithSizeGuard(
      "/abs/big.ifc",
      confirmStub,
    );

    expect(outcome.kind).toBe("imported");
    expect(checkSpy).toHaveBeenCalledOnce();
    expect(confirmStub).toHaveBeenCalledOnce();
    expect(confirmStub).toHaveBeenCalledWith({
      path: "/abs/big.ifc",
      sizeBytes: 412 * 1024 * 1024,
      thresholdBytes: 100 * 1024 * 1024,
    });
    expect(importSpy).toHaveBeenCalledOnce();
  });

  it("skips importIfc entirely when user declines the confirm", async () => {
    vi.spyOn(aec.bim, "checkFileSize").mockResolvedValue({
      path: "/abs/huge.ifc",
      fileSizeBytes: 500 * 1024 * 1024,
      largeFileWarning: true,
      thresholdBytes: 100 * 1024 * 1024,
    });
    const importSpy = vi.spyOn(aec.bim, "importIfc");
    const confirmStub = vi.fn(() => false); // user clicks Cancel

    const outcome = await importIfcWithSizeGuard(
      "/abs/huge.ifc",
      confirmStub,
    );

    expect(outcome.kind).toBe("cancelled-by-user");
    if (outcome.kind === "cancelled-by-user") {
      expect(outcome.sizeBytes).toBe(500 * 1024 * 1024);
      expect(outcome.thresholdBytes).toBe(100 * 1024 * 1024);
    }
    expect(confirmStub).toHaveBeenCalledOnce();
    expect(
      importSpy,
      "user-cancelled imports must never invoke the bridge parse",
    ).not.toHaveBeenCalled();
  });

  it("returns failed outcome when checkFileSize errors (no throw)", async () => {
    vi.spyOn(aec.bim, "checkFileSize").mockRejectedValue(
      new Error("ENOENT: no such file"),
    );
    const importSpy = vi.spyOn(aec.bim, "importIfc");

    const outcome = await importIfcWithSizeGuard("/missing.ifc");

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.error).toContain("ENOENT");
    }
    expect(importSpy).not.toHaveBeenCalled();
  });

  it("returns failed outcome when importIfc errors (no throw)", async () => {
    vi.spyOn(aec.bim, "checkFileSize").mockResolvedValue({
      path: "/abs/bad.ifc",
      fileSizeBytes: 1024,
      largeFileWarning: false,
      thresholdBytes: 100 * 1024 * 1024,
    });
    vi.spyOn(aec.bim, "importIfc").mockRejectedValue(
      new Error("malformed STEP record"),
    );

    const outcome = await importIfcWithSizeGuard("/abs/bad.ifc");

    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.error).toContain("malformed STEP record");
    }
  });

  it("supports async confirm callbacks", async () => {
    vi.spyOn(aec.bim, "checkFileSize").mockResolvedValue({
      path: "/abs/large.ifc",
      fileSizeBytes: 200 * 1024 * 1024,
      largeFileWarning: true,
      thresholdBytes: 100 * 1024 * 1024,
    });
    const importSpy = vi
      .spyOn(aec.bim, "importIfc")
      .mockResolvedValue(summary({ path: "/abs/large.ifc", elements: 1 }));
    // Simulate an async modal: the dialog resolves after a tick.
    const asyncConfirm = vi.fn(() => Promise.resolve(true));

    const outcome = await importIfcWithSizeGuard(
      "/abs/large.ifc",
      asyncConfirm,
    );

    expect(outcome.kind).toBe("imported");
    expect(asyncConfirm).toHaveBeenCalledOnce();
    expect(importSpy).toHaveBeenCalledOnce();
  });
});

describe("defaultLargeFileConfirm", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("calls window.confirm with a human-readable MB message", () => {
    const confirmSpy = vi
      .spyOn(window, "confirm")
      .mockImplementation(() => true);

    const result = defaultLargeFileConfirm({
      path: "/abs/large.ifc",
      sizeBytes: 412 * 1024 * 1024,
      thresholdBytes: 100 * 1024 * 1024,
    });

    expect(result).toBe(true);
    expect(confirmSpy).toHaveBeenCalledOnce();
    const message = confirmSpy.mock.calls[0][0];
    expect(message).toContain("412.0 MB");
    expect(message).toContain("100 MB");
    expect(message).toContain("/abs/large.ifc");
    expect(message).toContain("Continue?");
  });

  it("returns the user's choice verbatim (false = cancel)", () => {
    vi.spyOn(window, "confirm").mockImplementation(() => false);

    const result = defaultLargeFileConfirm({
      path: "/abs/large.ifc",
      sizeBytes: 200 * 1024 * 1024,
      thresholdBytes: 100 * 1024 * 1024,
    });

    expect(result).toBe(false);
  });
});
