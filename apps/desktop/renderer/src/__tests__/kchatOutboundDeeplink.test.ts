/**
 * Regression test for the sliding-window token bucket in
 * `kchatOutboundDeeplink.ts`.
 *
 * Closes Devin Review round-8 ANALYSIS_0001: the refill path
 * previously snapped `lastRefillMs = now` after computing the
 * refill count, silently discarding any sub-interval progress
 * (up to `REFILL_MS - 1` ms per refill). At the bucket's nominal
 * 500 ms cadence this caps the effective refill rate well below
 * the documented `CAPACITY / REFILL_MS`. The fix advances
 * `lastRefillMs` by exactly `refill * REFILL_MS` so accrued
 * fractional intervals carry into the next call.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  KCHAT_DEEPLINK_BUCKET_CAPACITY,
  KCHAT_DEEPLINK_BUCKET_REFILL_MS,
  __resetOutboundBucketForTesting,
  openKchatDeeplink,
} from "../../../electron/kchat/kchatOutboundDeeplink";

const BASELINE_MS = 1_000_000;

function fakeShell() {
  return { openExternal: vi.fn(async () => undefined) } satisfies {
    openExternal: (url: string) => Promise<void>;
  };
}

describe("openKchatDeeplink sliding-window refill", () => {
  beforeEach(() => {
    __resetOutboundBucketForTesting(BASELINE_MS);
  });

  afterEach(() => {
    __resetOutboundBucketForTesting();
  });

  it("preserves sub-interval progress across refills", () => {
    const shellModule = fakeShell();
    const drainAt = BASELINE_MS;
    // Drain the full bucket at the baseline timestamp. After the
    // loop we hold 0 tokens and `lastRefillMs` is still pinned to
    // `BASELINE_MS` (the elapsed delta is 0, so the refill branch
    // never fires).
    for (let i = 0; i < KCHAT_DEEPLINK_BUCKET_CAPACITY; i += 1) {
      const result = openKchatDeeplink("kchat://app/test", {
        shellModule,
        nowMs: () => drainAt,
      });
      expect(result).toEqual({ ok: true });
    }
    expect(shellModule.openExternal).toHaveBeenCalledTimes(
      KCHAT_DEEPLINK_BUCKET_CAPACITY,
    );

    // First refill at `drainAt + 1200` (elapsed = 1200 ms):
    //   refill = floor(1200 / 500) = 2
    //   tokens = min(CAPACITY, 0 + 2) = 2, then -1 on consume = 1
    //   With the fix: lastRefillMs += 2 * 500 = 1000  →  drainAt + 1000
    //   Without the fix: lastRefillMs = now             →  drainAt + 1200
    const firstRefillAt = drainAt + 2 * KCHAT_DEEPLINK_BUCKET_REFILL_MS + 200;
    const firstAfterRefill = openKchatDeeplink("kchat://app/test", {
      shellModule,
      nowMs: () => firstRefillAt,
    });
    expect(firstAfterRefill).toEqual({ ok: true });

    // Drain the remaining token in the same millisecond so the
    // sub-interval check below is forced to depend on a fresh
    // refill rather than leftover capacity from `firstAfterRefill`.
    const drainAfterRefill = openKchatDeeplink("kchat://app/test", {
      shellModule,
      nowMs: () => firstRefillAt,
    });
    expect(drainAfterRefill).toEqual({ ok: true });

    // 300 ms later. With the proportional update, total accounted
    // elapsed since `lastRefillMs = drainAt + 1000` is exactly
    // 500 ms — one refill window — so the bucket hands us a
    // token. With the broken `lastRefillMs = now` baseline the
    // elapsed would be only 300 ms, the refill branch never
    // fires, and the call comes back `rate_limited`. This
    // assertion is the load-bearing one for ANALYSIS_0001.
    const subIntervalAt = firstRefillAt + 300;
    const subIntervalResult = openKchatDeeplink("kchat://app/test", {
      shellModule,
      nowMs: () => subIntervalAt,
    });
    expect(subIntervalResult).toEqual({ ok: true });
    expect(shellModule.openExternal).toHaveBeenCalledTimes(
      KCHAT_DEEPLINK_BUCKET_CAPACITY + 3,
    );
  });

  it("caps recovered tokens at CAPACITY across very long gaps", () => {
    const shellModule = fakeShell();
    for (let i = 0; i < KCHAT_DEEPLINK_BUCKET_CAPACITY; i += 1) {
      openKchatDeeplink("kchat://app/test", {
        shellModule,
        nowMs: () => BASELINE_MS,
      });
    }
    // 10 refill windows elapsed; only `CAPACITY` tokens are
    // restored. The next `CAPACITY + 1` calls within the same
    // millisecond should produce `CAPACITY` successes and one
    // `rate_limited`.
    const wakeupAt = BASELINE_MS + 10 * KCHAT_DEEPLINK_BUCKET_REFILL_MS;
    const successes: boolean[] = [];
    for (let i = 0; i < KCHAT_DEEPLINK_BUCKET_CAPACITY + 1; i += 1) {
      const result = openKchatDeeplink("kchat://app/test", {
        shellModule,
        nowMs: () => wakeupAt,
      });
      successes.push(result.ok);
    }
    expect(successes).toEqual([
      ...Array.from({ length: KCHAT_DEEPLINK_BUCKET_CAPACITY }, () => true),
      false,
    ]);
  });

  it("rejects non-kchat schemes without consuming a token", () => {
    const shellModule = fakeShell();
    const denied = openKchatDeeplink("file:///etc/passwd", {
      shellModule,
      nowMs: () => BASELINE_MS,
    });
    expect(denied).toEqual({ ok: false, reason: "scheme_not_allowed" });
    expect(shellModule.openExternal).not.toHaveBeenCalled();
    // Bucket should still be at full capacity; drain it and
    // confirm every consume succeeded.
    for (let i = 0; i < KCHAT_DEEPLINK_BUCKET_CAPACITY; i += 1) {
      const result = openKchatDeeplink("kchat://app/test", {
        shellModule,
        nowMs: () => BASELINE_MS,
      });
      expect(result).toEqual({ ok: true });
    }
  });
});
