import { describe, expect, it } from "vitest";

import { inProcessBackend } from "../../../electron/bridge";

/**
 * Pins the in-process backend's `bimClassify` + `bimSetProperty`
 * stubs against the native service's validation contract
 * (`crates/aec_bridge/src/service.rs`):
 *
 *   - `bim_classify` validates the scheme string but does not
 *     whitespace-strip — `requireStringField` (typeof check only)
 *     is the right level of strictness here.
 *
 *   - `bim_set_property` rejects whitespace-only `pset` / `key` via
 *     `trim().is_empty()` at `service.rs:2209-2213`. The in-process
 *     stub must reject the same inputs with the same `must not be
 *     empty` wording, otherwise a dev-mode renderer could pass a
 *     whitespace pset / key through vitest only to crash in
 *     production once the `.node` artefact is loaded.
 *
 *     `entityId` and `value` are NOT whitespace-stripped by the
 *     native side — an empty `value` is a valid stored property,
 *     and `entityId` is checked by the entity-existence SELECT
 *     after the property write — so the in-process stub keeps
 *     `requireStringField` strictness on those.
 *
 * Devin Review PR-W round 5 flagged the parity gap: the in-process
 * stub's own comment claimed it "mirrors the native adapter for
 * dev/prod parity" but only checked `typeof v !== "string"`. This
 * file is the regression pin against re-introducing that gap.
 */
describe("bridge in-process bim methods", () => {
  it("bimClassify rejects missing scheme + projectPath", async () => {
    const b = inProcessBackend();
    await expect(
      b.bimClassify({ projectPath: "/tmp/p.aecstudio" }),
    ).rejects.toThrow(
      /bimClassify: missing required string field 'scheme'/,
    );
    await expect(b.bimClassify({ scheme: "ifc" })).rejects.toThrow(
      /bimClassify: params\.projectPath must be a non-empty string/,
    );
  });

  it("bimClassify echoes the supplied scheme + reports zero work for the stub", async () => {
    const b = inProcessBackend();
    const r = await b.bimClassify({
      projectPath: "/tmp/p.aecstudio",
      scheme: "uniformat-ii",
    });
    expect(r.scheme).toBe("uniformat-ii");
    expect(r.classified).toBe(0);
    expect(r.unchanged).toBe(0);
    expect(r.skipped).toBe(0);
    expect(r.details).toEqual([]);
  });

  it("bimSetProperty rejects missing entityId / pset / key / value", async () => {
    const b = inProcessBackend();
    const base = {
      projectPath: "/tmp/p.aecstudio",
      entityId: "ent_1",
      pset: "Pset_WallCommon",
      key: "FireRating",
      value: "120",
    };
    for (const field of ["entityId", "pset", "key", "value"] as const) {
      const params: Record<string, unknown> = { ...base };
      delete params[field];
      await expect(b.bimSetProperty(params)).rejects.toThrow(
        new RegExp(`bimSetProperty: missing required string field '${field}'`),
      );
    }
  });

  it("bimSetProperty rejects missing projectPath (separate `requireProjectPath` helper format)", async () => {
    const b = inProcessBackend();
    await expect(
      b.bimSetProperty({
        entityId: "ent_1",
        pset: "Pset_WallCommon",
        key: "FireRating",
        value: "120",
      }),
    ).rejects.toThrow(
      /bimSetProperty: params\.projectPath must be a non-empty string/,
    );
  });

  it("bimSetProperty rejects whitespace-only pset / key — matches native trim().is_empty() at service.rs:2209-2213", async () => {
    const b = inProcessBackend();
    const base = {
      projectPath: "/tmp/p.aecstudio",
      entityId: "ent_1",
      pset: "Pset_WallCommon",
      key: "FireRating",
      value: "120",
    };
    for (const blank of ["", "   ", "\t", "\n", "  \t\n "]) {
      await expect(
        b.bimSetProperty({ ...base, pset: blank }),
      ).rejects.toThrow(/bimSetProperty: pset must not be empty/);
      await expect(
        b.bimSetProperty({ ...base, key: blank }),
      ).rejects.toThrow(/bimSetProperty: key must not be empty/);
    }
  });

  it("bimSetProperty accepts an empty value (native does too — empty string is a valid property body)", async () => {
    const b = inProcessBackend();
    const r = await b.bimSetProperty({
      projectPath: "/tmp/p.aecstudio",
      entityId: "ent_1",
      pset: "Pset_WallCommon",
      key: "FireRating",
      value: "",
    });
    expect(r.entityId).toBe("ent_1");
    expect(r.pset).toBe("Pset_WallCommon");
    expect(r.key).toBe("FireRating");
    expect(r.previousValue).toBeNull();
  });

  it("bimSetProperty echoes a happy-path call", async () => {
    const b = inProcessBackend();
    const r = await b.bimSetProperty({
      projectPath: "/tmp/p.aecstudio",
      entityId: "ent_1",
      pset: "Pset_WallCommon",
      key: "FireRating",
      value: "120",
    });
    expect(r.entityId).toBe("ent_1");
    expect(r.pset).toBe("Pset_WallCommon");
    expect(r.key).toBe("FireRating");
    expect(r.previousValue).toBeNull();
  });
});
