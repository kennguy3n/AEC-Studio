import * as fs from "node:fs";
import * as path from "node:path";

import { describe, expect, it } from "vitest";

import {
  BUILT_IN_PRESET_IDS,
  NATIVE_FALLBACK_METHODS,
  NATIVE_WIRED_METHODS,
  inProcessBackend,
} from "../../../electron/bridge";

/**
 * The two catalogues exported by `bridge.ts` (`NATIVE_WIRED_METHODS` and
 * `NATIVE_FALLBACK_METHODS`) must, together, cover every method on the
 * `BridgeBackend` interface. `adaptNative` enforces this at runtime by
 * throwing on missing declarations; this test enforces the same
 * invariant at test time so the failure surfaces in CI rather than
 * after the native binary lands.
 *
 * Adding a new method to `BridgeBackend` requires adding it to exactly
 * one of these two lists.
 */
describe("bridge catalogue", () => {
  it("WIRED \u222a FALLBACK covers every BridgeBackend method", () => {
    const backend = inProcessBackend();
    const declared = new Set<string>([
      ...NATIVE_WIRED_METHODS,
      ...NATIVE_FALLBACK_METHODS,
    ]);
    const missing = Object.keys(backend).filter((k) => !declared.has(k));
    expect(missing).toEqual([]);
  });

  it("WIRED and FALLBACK lists are disjoint", () => {
    const wired = new Set<string>(NATIVE_WIRED_METHODS);
    const overlap = NATIVE_FALLBACK_METHODS.filter((k) => wired.has(k));
    expect(overlap).toEqual([]);
  });

  /**
   * Cross-language pin for the built-in render preset ids — see the
   * doc comment on `BUILT_IN_PRESET_IDS` in `bridge.ts` and the
   * companion Rust integration test
   * `crates/aec_render/tests/preset_ids.rs`. The fixture
   * `crates/aec_render/tests/preset_ids.json` is regenerated from
   * `RenderPreset::defaults()` via:
   *
   *   AEC_UPDATE_PRESET_IDS=1 cargo test -p aec_render --test preset_ids
   *
   * If this test fails, the TS-side `BUILT_IN_PRESET_IDS` constant
   * drifted from the Rust source of truth. Add or remove the missing
   * id (in the constant *and* in the renderer-side preset selector)
   * to bring it back into lockstep.
   */
  it("BUILT_IN_PRESET_IDS matches the native RenderPresetStore", () => {
    const fixturePath = path.resolve(
      __dirname,
      "../../../../../crates/aec_render/tests/preset_ids.json",
    );
    const expectedSorted = JSON.parse(
      fs.readFileSync(fixturePath, "utf-8"),
    ) as string[];
    const actualSorted = [...BUILT_IN_PRESET_IDS].sort();
    expect(actualSorted).toEqual(expectedSorted);
  });
});
