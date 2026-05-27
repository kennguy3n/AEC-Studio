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

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const s = (await aec.kchat.status()) as KChatStatus;
        if (!cancelled) setStatus(s);
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

  const instance = parseInstance(status.instanceJson);

  return (
    <button
      type="button"
      data-testid="kchat-status-chip"
      data-state={status.state}
      className={`status-bar__chip is-kchat is-${status.state}`}
      onClick={onReload}
      disabled={pending}
      aria-label={`KChat: ${stateLabel[status.state]} (click to reload)`}
      title={
        instance
          ? `KChat Desktop ${instance.version} at ${instance.socket_path}`
          : "No KChat Desktop instance detected"
      }
    >
      KChat · {stateLabel[status.state]}
    </button>
  );
}

function parseInstance(json: string | null): {
  socket_path: string;
  version: string;
  health: string;
} | null {
  if (!json) return null;
  try {
    const parsed = JSON.parse(json) as {
      socket_path?: string;
      version?: string;
      health?: string;
    };
    if (!parsed.socket_path || !parsed.version) return null;
    return {
      socket_path: parsed.socket_path,
      version: parsed.version,
      health: parsed.health ?? "unknown",
    };
  } catch {
    return null;
  }
}
