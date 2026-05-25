import { describe, expect, it } from "vitest";

import { inProcessBackend } from "../../../electron/bridge";

/**
 * The in-process backend exposes `projectEngineStatus` and
 * `projectAuditSync` as a Phase 9 contract: every renderer call site
 * must work against either the native bridge or the fallback. The
 * fallback returns a stable "zero-state" shape — these tests pin that
 * shape so a renderer component built against the in-process backend
 * doesn't break when wired through to the native one.
 *
 * Drift between the two backends is enforced by the catalogue tests
 * (see `bridge-catalogue.test.ts`); this file pins the EngineStatus
 * shape itself.
 */
describe("bridge in-process engine status", () => {
  it("projectEngineStatus returns zero-state with all five canonical scopes", async () => {
    const bridge = inProcessBackend();
    const status = await bridge.projectEngineStatus("/projects/anything.aecstudio");
    expect(status.schemaVersion).toBe(1);
    expect(status.auditEntryCount).toBe(0);
    expect(status.auditChainSqlCount).toBe(0);
    // All five canonical scopes present at 0.
    for (const scope of ["design", "draft", "bim", "render", "deliver"] as const) {
      expect(status.auditChainByScope[scope]).toBe(0);
    }
  });

  it("projectAuditSync is a no-op in the in-process fallback", async () => {
    const bridge = inProcessBackend();
    const n = await bridge.projectAuditSync("/projects/anything.aecstudio");
    expect(n).toBe(0);
  });

  it("projectAuditVerify returns a trivially-ok report in the in-process fallback", async () => {
    const bridge = inProcessBackend();
    const v = await bridge.projectAuditVerify("/projects/anything.aecstudio");
    expect(v.status).toBe("ok");
    expect(v.entriesChecked).toBe(0);
    expect(v.filesChecked).toEqual([]);
    expect(v.headHash).toBe("blake3:genesis");
    expect(v.breakFile).toBeNull();
    expect(v.breakLine).toBeNull();
    expect(v.breakReason).toBeNull();
    expect(v.breakDetail).toBeNull();
  });
});
