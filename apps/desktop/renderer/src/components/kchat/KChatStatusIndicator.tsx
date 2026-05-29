import { useEffect, useState } from "react";
import type { AecApi } from "../../../../electron/preload";
import { aec } from "../../api/aec";

/**
 * Status chip for the StatusBar.
 *
 * Polls the bridge's `kchat:status` endpoint every 5 s and shows
 * the current connection state. Three visual states:
 *
 * - `connected` — a discovered KChat Desktop instance answered the
 *   last heartbeat. Displays the instance version, when available.
 * - `reconnecting` — we have a publisher but the last heartbeat
 *   failed; the transport retries with exponential backoff.
 * - `disconnected` — no instance discovered. The bridge falls
 *   back to an in-memory publisher (publishes still succeed but
 *   nothing leaves the box).
 *
 * Click the chip to trigger an immediate re-probe via
 * `kchat:reload`. Useful when KChat Desktop started up after AEC
 * Studio and the user wants to wire up the connection without
 * waiting for the next 5-s poll tick.
 */
// Derived from the IPC contract in preload.ts so adding a field to
// `kchat:status` (e.g. `defaultThreadId`) doesn't silently get
// stripped here. The chip only renders `state`, `publisherKind`,
// and `instanceJson` — `defaultThreadId` is consumed by `Deliver`
// — but typing the full payload keeps the contract honest and the
// `as KChatStatus` casts on lines 39 / 58 stop being implicit
// subset coercions.
export type KChatStatus = Awaited<ReturnType<AecApi["kchat"]["status"]>>;

const POLL_INTERVAL_MS = 5_000;

export function KChatStatusIndicator() {
  const [status, setStatus] = useState<KChatStatus | null>(null);
  const [pending, setPending] = useState(false);
  // Last-reload error message. Surfaced via the chip's `title`
  // tooltip and a `data-reload-error` attribute (the chip itself is
  // intentionally compact — no inline banner). The next successful
  // status poll or reload clears it so a one-off transport hiccup
  // doesn't leave a permanent error indicator.
  const [reloadError, setReloadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const s = (await aec.kchat.status()) as KChatStatus;
        if (!cancelled) {
          setStatus(s);
          // A successful background poll means the bridge is
          // healthy again; clear any stale reload-error tooltip so
          // the chip doesn't stay "stuck" after a transient
          // failure recovers on its own.
          setReloadError(null);
        }
      } catch {
        // Status poll failures are intentionally swallowed — the
        // chip continues showing its last-known state rather than
        // flickering to an error indicator on a single missed poll.
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  const onReload = async () => {
    setPending(true);
    try {
      const s = (await aec.kchat.reload()) as KChatStatus;
      setStatus(s);
      // Successful reload clears any prior error tooltip.
      setReloadError(null);
    } catch (e) {
      // `kchat:reload` can reject when the bridge is mid-restart,
      // the discovered socket is unreachable, or napi serialization
      // fails. Without this catch the rejection escapes as an
      // unhandled promise (React ignores Promises returned from
      // `onClick`), the user sees no feedback, and the chip would
      // stay stuck `disabled` if the `finally` didn't reset it.
      // Capturing the message gives the user a tooltip explaining
      // *why* the click had no visible effect; the next successful
      // status poll or reload clears it.
      const msg = e instanceof Error ? e.message : String(e);
      setReloadError(msg);
    } finally {
      setPending(false);
    }
  };

  if (!status) {
    return (
      <span
        data-testid="kchat-status-chip"
        className="status-bar__chip is-kchat-loading"
      >
        KChat · …
      </span>
    );
  }

  const stateLabel: Record<KChatStatus["state"], string> = {
    connected: "online",
    reconnecting: "reconnecting…",
    disconnected: "offline",
  };

  // The bridge-persisted master toggle has priority over the
  // transport state — when the integration is disabled, the
  // Electron `kchat:publish` IPC handler refuses publishes
  // regardless of whether the loopback server / extension are up,
  // so reading "offline" here would be misleadingly transport-
  // sounding. Surface "disabled" explicitly so the user knows
  // they need to flip the Settings toggle, not restart KChat.
  const displayLabel = status.enabled
    ? stateLabel[status.state]
    : "disabled";

  const instance = parseInstance(status.instanceJson);
  // Phase 15: surface the loopback API port + heartbeat instead
  // of the socket path + version. "never" reads better in a
  // tooltip than the wire-level `null`.
  const baseTitle = !status.enabled
    ? "KChat integration is disabled — enable it in Settings to start publishing"
    : instance
      ? `KChat loopback API on 127.0.0.1:${instance.apiServerPort ?? "?"} · last extension heartbeat ${instance.lastExtensionContactAt ?? "never"}`
      : "KChat loopback API not running";
  const title = reloadError
    ? `Reload failed: ${reloadError}\n\n${baseTitle}`
    : baseTitle;
  const ariaLabel = reloadError
    ? `KChat: ${displayLabel} — reload failed: ${reloadError} (click to retry)`
    : `KChat: ${displayLabel} (click to reload)`;

  return (
    <button
      type="button"
      data-testid="kchat-status-chip"
      data-state={status.state}
      data-enabled={status.enabled ? "true" : "false"}
      data-reload-error={reloadError ?? undefined}
      className={`status-bar__chip is-kchat is-${status.state}${status.enabled ? "" : " is-disabled"}`}
      onClick={onReload}
      disabled={pending}
      aria-label={ariaLabel}
      title={title}
    >
      KChat · {displayLabel}
    </button>
  );
}

/**
 * Phase 15: loopback-API snapshot shape returned by
 * `kchat:status`'s `instanceJson`. Mirrors
 * `KchatRendererSnapshot` from `kchatAppState.ts`.
 */
interface KChatLoopbackInstance {
  apiServerRunning: boolean;
  apiServerPort: number | null;
  portFilePath: string | null;
  lastExtensionContactAt: string | null;
  queuedPublishCount: number;
  reviewThreadCount: number;
}

function parseInstance(
  json: string | null,
): KChatLoopbackInstance | null {
  if (!json) return null;
  try {
    const parsed = JSON.parse(json) as Partial<KChatLoopbackInstance>;
    if (
      typeof parsed.apiServerRunning !== "boolean" ||
      typeof parsed.queuedPublishCount !== "number" ||
      typeof parsed.reviewThreadCount !== "number"
    ) {
      return null;
    }
    return {
      apiServerRunning: parsed.apiServerRunning,
      apiServerPort:
        typeof parsed.apiServerPort === "number" ? parsed.apiServerPort : null,
      portFilePath:
        typeof parsed.portFilePath === "string" ? parsed.portFilePath : null,
      lastExtensionContactAt:
        typeof parsed.lastExtensionContactAt === "string"
          ? parsed.lastExtensionContactAt
          : null,
      queuedPublishCount: parsed.queuedPublishCount,
      reviewThreadCount: parsed.reviewThreadCount,
    };
  } catch {
    return null;
  }
}
