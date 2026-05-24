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
 * These tests pin three invariants for the in-process backend:
 *
 *   1. `outPath` round-trips from the renderer-supplied params
 *      (rather than always being a fixed `"/exports/out.pdf"`).
 *   2. `exportPdf.pages` is `2`, matching
 *      `aec_export::write_project_pdf` (1 cover + 1 overview)
 *      — so a renderer test that asserts "Exported 2 pages" works
 *      against both backends.
 *   3. `outPath` + `projectName` are **mandatory** on every
 *      `export*` method — the in-process backend rejects missing
 *      fields with the same error shape the native napi struct
 *      validation produces, so a renderer call like
 *      `exportPdf({})` fails identically in vitest + production.
 *
 * Catalogue + adapter invariants are covered by
 * `bridge-catalogue.test.ts`; this file adds the per-method
 * behavioural pin.
 */
describe("bridge in-process export methods", () => {
  it("exportPdf honours outPath + projectName and returns pages=2", async () => {
    const b = inProcessBackend();
    const r = await b.exportPdf({
      outPath: "/tmp/my-project.pdf",
      projectName: "My Project",
    });
    expect(r.outPath).toBe("/tmp/my-project.pdf");
    expect(r.pages).toBe(2);
  });

  it("exportDxf honours outPath + projectName", async () => {
    const b = inProcessBackend();
    const r = await b.exportDxf({
      outPath: "/tmp/my-project.dxf",
      projectName: "My Project",
    });
    expect(r.outPath).toBe("/tmp/my-project.dxf");
  });

  it("exportIfc honours outPath + projectName", async () => {
    const b = inProcessBackend();
    const r = await b.exportIfc({
      outPath: "/tmp/my-project.ifc",
      projectName: "My Project",
    });
    expect(r.outPath).toBe("/tmp/my-project.ifc");
  });

  it("exportGltf honours outPath + projectName", async () => {
    const b = inProcessBackend();
    const r = await b.exportGltf({
      outPath: "/tmp/my-project.gltf",
      projectName: "My Project",
    });
    expect(r.outPath).toBe("/tmp/my-project.gltf");
  });

  it("exportBuildProposalPack honours outPath + projectName", async () => {
    const b = inProcessBackend();
    const r = await b.exportBuildProposalPack({
      outPath: "/tmp/proposal.pdf",
      projectName: "My Project",
    });
    expect(r.outPath).toBe("/tmp/proposal.pdf");
  });

  it("export.* methods reject missing outPath the same way the native napi struct does", async () => {
    const b = inProcessBackend();
    await expect(b.exportPdf({})).rejects.toThrow(
      /exportPdf: missing required string field 'outPath'/,
    );
    await expect(b.exportDxf({})).rejects.toThrow(
      /exportDxf: missing required string field 'outPath'/,
    );
    await expect(b.exportIfc({})).rejects.toThrow(
      /exportIfc: missing required string field 'outPath'/,
    );
    await expect(b.exportGltf({})).rejects.toThrow(
      /exportGltf: missing required string field 'outPath'/,
    );
    await expect(b.exportBuildProposalPack({})).rejects.toThrow(
      /exportBuildProposalPack: missing required string field 'outPath'/,
    );
  });

  it("export.* methods reject missing projectName the same way the native napi struct does", async () => {
    const b = inProcessBackend();
    await expect(
      b.exportPdf({ outPath: "/tmp/a.pdf" }),
    ).rejects.toThrow(
      /exportPdf: missing required string field 'projectName'/,
    );
    await expect(
      b.exportDxf({ outPath: "/tmp/a.dxf" }),
    ).rejects.toThrow(
      /exportDxf: missing required string field 'projectName'/,
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

  it("deliverBuildPack omitted include_* flags default to ON (matches native unwrap_or(true))", async () => {
    // Pins the JS in-process backend `?? true` default + the native
    // `unwrap_or(true)` parity at the napi boundary. When a renderer
    // omits the optional booleans, the full kind-appropriate
    // inventory must appear in `contents` — otherwise concept packs
    // silently lose `renders/01_cover.png` etc. when the native
    // backend is loaded. See Devin Review PR-S round 1 BUG_0001
    // ("deliver_build_pack inventory flags default to false in native
    // backend but true in JS in-process backend").
    const b = inProcessBackend();
    const concept = await b.deliverBuildPack({
      kind: "concept",
      outPath: "/tmp/concept-pack.zip",
    });
    expect(concept.contents).toEqual([
      "concept_pack.pdf",
      "sheets/A100.pdf",
      "renders/01_cover.png",
      "manifest.json",
    ]);
    const contractor = await b.deliverBuildPack({
      kind: "contractor",
      outPath: "/tmp/contractor-pack.zip",
    });
    expect(contractor.contents).toEqual([
      "contractor_summary.pdf",
      "sheets/A100.pdf",
      "sheets/A101.pdf",
      "schedules/materials.xlsx",
      "schedules/boq.xlsx",
      "model/project.ifc",
      "proposal.pdf",
      "manifest.json",
    ]);
    const interior = await b.deliverBuildPack({
      kind: "interior",
      outPath: "/tmp/interior-pack.zip",
    });
    expect(interior.contents).toEqual([
      "interior_summary.pdf",
      "renders/01_living.png",
      "renders/02_kitchen.png",
      "schedules/materials.xlsx",
      "manifest.json",
    ]);
    const bim = await b.deliverBuildPack({
      kind: "bim",
      outPath: "/tmp/bim-pack.zip",
    });
    expect(bim.contents).toEqual([
      "validation_report.pdf",
      "sheets/A100.pdf",
      "sheets/A101.pdf",
      "model/project.ifc",
      "manifest.json",
    ]);
  });
});
