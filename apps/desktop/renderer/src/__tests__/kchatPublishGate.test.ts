import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  resolveEnabledAndDefaultThreadFromBridge,
  resolveEnabledAndDefaultThreadFromBridgeStrict,
} from "../../../electron/ipc";
import { setBridge, type BridgeBackend } from "../../../electron/bridge";

/**
 * Regression tests for the round-9 BUG_0001 publish-gate fix.
 *
 * Two helpers wrap the bridge's `kchatStatus()` snapshot for IPC
 * handlers that need both the master `enabled` flag and the
 * per-project `defaultThreadId`:
 *
 *  - The **soft** helper (used by `kchat:status` / `kchat:reload`)
 *    fails open: a transient bridge error returns
 *    `{ enabled: true, defaultThreadId: null }` so the status
 *    indicator chip stays meaningful at boot.
 *  - The **strict** helper (used by `kchat:publish` /
 *    `kchat:ingestReviews`) fails closed: a bridge error
 *    propagates so a transient bridge blip cannot silently
 *    override an explicit user disable on a write path or
 *    data-exposure path.
 *
 * The previous (single) helper was used everywhere and defaulted
 * to `enabled: true`, which meant `kchat:publish` would happily
 * accept a publish whenever the bridge was briefly unavailable —
 * even when the user had explicitly toggled the integration off
 * in Settings. Devin Review round-9 BUG_0001 caught this; the
 * fix is the two-variant split exercised below.
 */

interface MockableBridge extends Partial<BridgeBackend> {
  kchatStatus: BridgeBackend["kchatStatus"];
}

function installMockBridge(impl: MockableBridge): void {
  setBridge(impl as BridgeBackend);
}

describe("kchat publish gate — soft vs strict bridge helpers", () => {
  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("strict helper propagates bridge errors (fail closed for publish path)", async () => {
    installMockBridge({
      kchatStatus: vi
        .fn()
        .mockRejectedValue(new Error("bridge.node not loaded")),
    });
    await expect(resolveEnabledAndDefaultThreadFromBridgeStrict()).rejects.toThrow(
      "bridge.node not loaded",
    );
  });

  it("soft helper swallows bridge errors and defaults to enabled=true (status display)", async () => {
    installMockBridge({
      kchatStatus: vi
        .fn()
        .mockRejectedValue(new Error("bridge.node not loaded")),
    });
    const result = await resolveEnabledAndDefaultThreadFromBridge();
    expect(result).toEqual({ enabled: true, defaultThreadId: null });
  });

  it("strict helper returns the bridge snapshot when enabled=false", async () => {
    // The whole point of failing closed on bridge errors is that
    // when the bridge IS reachable, an explicit user disable
    // reaches the publish gate. This case is the production
    // happy path for the disable-then-publish sequence.
    installMockBridge({
      kchatStatus: vi.fn().mockResolvedValue({
        state: "disconnected",
        publisherKind: "loopback_http",
        instanceJson: null,
        defaultThreadId: "proj-thread-7",
        enabled: false,
      }),
    });
    const strict = await resolveEnabledAndDefaultThreadFromBridgeStrict();
    expect(strict).toEqual({
      enabled: false,
      defaultThreadId: "proj-thread-7",
    });
  });

  it("strict helper coerces undefined defaultThreadId to null", async () => {
    installMockBridge({
      kchatStatus: vi.fn().mockResolvedValue({
        state: "connected",
        publisherKind: "loopback_http",
        instanceJson: null,
        enabled: true,
      }),
    });
    const strict = await resolveEnabledAndDefaultThreadFromBridgeStrict();
    expect(strict).toEqual({ enabled: true, defaultThreadId: null });
  });

  it("soft helper returns the bridge snapshot when the bridge is reachable", async () => {
    installMockBridge({
      kchatStatus: vi.fn().mockResolvedValue({
        state: "connected",
        publisherKind: "loopback_http",
        instanceJson: null,
        defaultThreadId: "proj-thread-42",
        enabled: true,
      }),
    });
    const soft = await resolveEnabledAndDefaultThreadFromBridge();
    expect(soft).toEqual({
      enabled: true,
      defaultThreadId: "proj-thread-42",
    });
  });
});
