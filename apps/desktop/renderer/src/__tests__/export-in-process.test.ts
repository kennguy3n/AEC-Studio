import { describe, expect, it } from "vitest";

import {
  NATIVE_WIRED_METHODS,
  inProcessBackend,
} from "../../../electron/bridge";

/**
 * PR-S pins the export.* + deliver.* IPC surface against the
 * in-process backend. The native bridge handles the real file
 * writes (see `aec_export::write_*` in `crates/aec_export/src/
 * project_export.rs`); the in-process backend is what vitest +
 * dev mode (no `.node` artefact) exercise.
 *
 * These tests pin two invariants for the in-process backend:
 *
 *   1. `outPath` round-trips from the renderer-supplied params
 *      (rather than always being a fixed `"/exports/out.pdf"`).
 *   2. `exportPdf.pages` is `2`, matching
 *      `aec_export::write_project_pdf` (1 cover + 1 overview)
 *      — so a renderer test that asserts "Exported 2 pages" works
 *      against both backends.
 *
 * Catalogue + adapter invariants are covered by
 * `bridge-catalogue.test.ts`; this file adds the per-method
 * behavioural pin.
 */
describe("bridge in-process export methods", () => {
  it("exportPdf honours outPath and returns pages=2", async () => {
    const b = inProcessBackend();
    const r = await b.exportPdf({ outPath: "/tmp/my-project.pdf" });
    expect(r.outPath).toBe("/tmp/my-project.pdf");
    expect(r.pages).toBe(2);
  });

  it("exportDxf honours outPath", async () => {
    const b = inProcessBackend();
    const r = await b.exportDxf({ outPath: "/tmp/my-project.dxf" });
    expect(r.outPath).toBe("/tmp/my-project.dxf");
  });

  it("exportIfc honours outPath", async () => {
    const b = inProcessBackend();
    const r = await b.exportIfc({ outPath: "/tmp/my-project.ifc" });
    expect(r.outPath).toBe("/tmp/my-project.ifc");
  });

  it("exportGltf honours outPath", async () => {
    const b = inProcessBackend();
    const r = await b.exportGltf({ outPath: "/tmp/my-project.gltf" });
    expect(r.outPath).toBe("/tmp/my-project.gltf");
  });

  it("exportBuildProposalPack honours outPath", async () => {
    const b = inProcessBackend();
    const r = await b.exportBuildProposalPack({
      outPath: "/tmp/proposal.pdf",
    });
    expect(r.outPath).toBe("/tmp/proposal.pdf");
  });

  it("export.* methods default to /exports/* paths when outPath is missing", async () => {
    const b = inProcessBackend();
    expect((await b.exportPdf({})).outPath).toBe("/exports/out.pdf");
    expect((await b.exportDxf({})).outPath).toBe("/exports/out.dxf");
    expect((await b.exportIfc({})).outPath).toBe("/exports/out.ifc");
    expect((await b.exportGltf({})).outPath).toBe("/exports/out.gltf");
    expect((await b.exportBuildProposalPack({})).outPath).toBe(
      "/exports/proposal.pdf",
    );
  });

  it("export.* + deliverBuildPack are listed in NATIVE_WIRED_METHODS (post PR-S)", () => {
    const wired = new Set<string>(NATIVE_WIRED_METHODS);
    for (const method of [
      "exportPdf",
      "exportDxf",
      "exportIfc",
      "exportGltf",
      "exportBuildProposalPack",
      "deliverBuildPack",
    ] as const) {
      expect(wired.has(method)).toBe(true);
    }
  });

  it("deliverBuildPack in-process backend reports a non-empty contents list", async () => {
    const b = inProcessBackend();
    const r = await b.deliverBuildPack({
      kind: "concept",
      outPath: "/tmp/concept-pack.zip",
    });
    expect(r.outPath).toBe("/tmp/concept-pack.zip");
    expect(r.contents.length).toBeGreaterThan(0);
    expect(r.totalBytes).toBeGreaterThan(0);
  });
});
