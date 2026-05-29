/**
 * Outbound `kchat://app/...` deeplinks from the AEC Studio
 * renderer (e.g. the PublishCardModal's "Open in KChat Desktop"
 * affordance after a successful publish).
 *
 * Rate-limited: a sliding-window bucket caps the renderer at
 * `CAPACITY` shell.openExternal calls per `REFILL_MS`. A normal
 * user clicks at most a few times per second; a runaway
 * renderer bug attempting to flood the OS shell with URLs hits
 * the bucket before any harm is done.
 *
 * Scheme allow-list: only `kchat://` URLs are forwarded.
 * `shell.openExternal` is otherwise happy to hand the OS shell
 * a `file://` / `data:` URL, which would be a renderer-driven
 * local-resource read — a privilege escalation across the
 * sandbox boundary.
 */

import { shell } from "electron";

/** Maximum tokens in the bucket. */
export const KCHAT_DEEPLINK_BUCKET_CAPACITY = 4;
/** Milliseconds it takes to recover one token. */
export const KCHAT_DEEPLINK_BUCKET_REFILL_MS = 500;

let tokens = KCHAT_DEEPLINK_BUCKET_CAPACITY;
let lastRefillMs = Date.now();

export interface OpenKchatDeeplinkResult {
  ok: boolean;
  reason?: "rate_limited" | "scheme_not_allowed";
}

/**
 * Validate + open a `kchat://` URL via Electron's
 * `shell.openExternal`. The result is synchronous so the IPC
 * handler can return a typed value to the renderer without
 * blocking on the OS shell.
 */
export function openKchatDeeplink(
  rawUrl: string,
  opts: {
    shellModule?: Pick<typeof shell, "openExternal">;
    nowMs?: () => number;
  } = {},
): OpenKchatDeeplinkResult {
  const shellModule = opts.shellModule ?? shell;
  const now = (opts.nowMs ?? Date.now)();
  // Sliding-window refill: every `REFILL_MS` since the last
  // call, recover one token.
  const elapsed = now - lastRefillMs;
  if (elapsed >= KCHAT_DEEPLINK_BUCKET_REFILL_MS) {
    const refill = Math.floor(elapsed / KCHAT_DEEPLINK_BUCKET_REFILL_MS);
    tokens = Math.min(KCHAT_DEEPLINK_BUCKET_CAPACITY, tokens + refill);
    lastRefillMs = now;
  }
  if (tokens <= 0) {
    return { ok: false, reason: "rate_limited" };
  }
  if (
    typeof rawUrl !== "string" ||
    !rawUrl.toLowerCase().startsWith("kchat://")
  ) {
    return { ok: false, reason: "scheme_not_allowed" };
  }
  tokens -= 1;
  shellModule.openExternal(rawUrl).catch((err) => {
    console.warn("[kchatOutboundDeeplink] shell.openExternal failed:", err);
  });
  return { ok: true };
}

/** Reset for unit tests. */
export function __resetOutboundBucketForTesting(): void {
  tokens = KCHAT_DEEPLINK_BUCKET_CAPACITY;
  lastRefillMs = Date.now();
}
