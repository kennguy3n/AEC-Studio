import { describe, expect, it } from "vitest";

import {
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
});
