import { useCallback, useEffect, useRef, useState } from "react";
import { aec } from "../../api/aec";

/**
 * Deliver-mode panel that shows review comments ingested from the
 * KChat thread tied to the active project.
 *
 * The panel polls `kchat:ingestReviews` on a 30 s interval, applying
 * the `since_iso` cursor advance so only newly-posted comments are
 * fetched per tick. Each comment renders with:
 *
 * - author (KChat handle)
 * - posted_at timestamp (renderer-local time zone)
 * - the comment body
 * - the artifact card it threads under (when present)
 *
 * Empty thread → friendly empty state; offline → callout pointing
 * the user at the StatusBar chip's "click to reload" affordance.
 */
export type KChatReviewPanelProps = {
  threadId: string;
  /**
   * When `true`, the panel renders even if the bridge reports the
   * in-memory fallback. Useful for screenshots / Storybook so the
   * empty state doesn't dominate the layout. Defaults to `false`,
   * which hides the panel entirely outside of an active local-ipc
   * connection.
   */
  showWhenOffline?: boolean;
};

export type ReviewCommentRow = {
  comment_id: string;
  author: string;
  text: string;
  posted_at: string;
  artifact_id: string | null;
};

const POLL_INTERVAL_MS = 30_000;

export function KChatReviewPanel(props: KChatReviewPanelProps) {
  const [comments, setComments] = useState<ReviewCommentRow[]>([]);
  const [offline, setOffline] = useState(false);
  const [loading, setLoading] = useState(false);
  // Last poll error message, if any. Surfaced inline below the
  // refresh button so the user knows *why* the panel is stale,
  // rather than silently presenting old comments. Cleared on the
  // next successful poll. Holding this in state (rather than
  // re-throwing) is deliberate: we need the component to keep its
  // last-known-good comment list visible, and we need
  // `void fetchOnce()` at the call sites not to produce
  // unhandled promise rejections every 30 s when the bridge is
  // misbehaving.
  const [error, setError] = useState<string | null>(null);
  // The `since_iso` cursor lives in a ref (not state) on purpose: it
  // advances on every successful ingest, and storing it in state would
  // churn the `fetchOnce` callback identity on every batch of new
  // comments. That in turn would tear down and re-create the 30 s
  // polling interval inside the effect below, producing a duplicate
  // round-trip and resetting the timer every time fresh reviews land.
  // Reading via a ref keeps the callback stable, the interval steady,
  // and the polling cadence honest.
  const sinceIsoRef = useRef<string | null>(null);

  // Tracks the threadId that is currently "active". fetchOnce captures
  // `props.threadId` at call time and checks this ref after every await;
  // if they diverge, a threadId switch happened mid-flight and the
  // response belongs to the old thread — it is silently discarded.
  const threadIdRef = useRef(props.threadId);

  // Reset all accumulated per-thread state when the parent passes a
  // different `threadId` (e.g. the user opened a project whose
  // `KChatConfig::default_thread_id` differs from the previous
  // project's). Two things would otherwise leak across the switch:
  //
  // 1. `sinceIsoRef` would still point at the newest comment from the
  //    *previous* thread, so the very next `kchat:ingestReviews` poll
  //    would ask the bridge for comments newer than a timestamp that
  //    belongs to a completely different thread — silently skipping
  //    every comment on the new thread older than that cursor until a
  //    full app reload.
  // 2. `comments` would still hold the previous thread's rows. The
  //    next `mergeAndSort` deduplicates by `comment_id` (UUIDs from
  //    different threads never collide), so old comments would simply
  //    persist alongside the new thread's, mixing two conversations
  //    in one UI.
  //
  // Clearing here — *before* the polling effect re-runs `fetchOnce`
  // (whose identity also flips on `props.threadId`) — guarantees the
  // next poll starts from a clean slate. We also clear `error` so a
  // banner from the previous thread doesn't bleed into the new one
  // before its first poll completes.
  useEffect(() => {
    threadIdRef.current = props.threadId;
    sinceIsoRef.current = null;
    setComments([]);
    setError(null);
  }, [props.threadId]);

  const fetchOnce = useCallback(async () => {
    // Capture the threadId at call time so we can detect a mid-flight
    // switch after each await below.
    const capturedThread = props.threadId;
    setLoading(true);
    try {
      const statusReport = await aec.kchat.status();
      if (threadIdRef.current !== capturedThread) return;
      // Phase 15 offline gate. Two independent signals collapse
      // the panel to its offline placeholder:
      //
      //   1. The bridge-persisted master toggle is off
      //      (`enabled === false`). The Settings card flipped it
      //      or the per-project manifest never opted in. The
      //      `kchat:publish` IPC handler rejects publishes in
      //      this state, so ingesting reviews would be polling
      //      an integration the user has explicitly disabled.
      //
      //   2. The transport state is `disconnected`. In Phase 15
      //      `buildKchatStatusResponse` only emits this when the
      //      master toggle is off *or* the loopback API server
      //      itself isn't running (boot failure / shutdown). We
      //      still keep polling on `reconnecting` (server up,
      //      no extension heartbeat yet) because the .kcz
      //      extension may come online mid-session.
      //
      // We deliberately do NOT branch on `publisherKind` here:
      // `buildKchatStatusResponse` hard-codes it to
      // `"loopback_http"` for every production payload, so any
      // condition that conjoined it would be dead in production.
      // The two signals above are the authoritative source of
      // truth — `state === "disconnected"` already covers the
      // server-not-running case, so this stays honest whether
      // the Rust-side `KChatState::publisher_kind` is `loopback`
      // or `in_memory`.
      const isOffline =
        !statusReport.enabled || statusReport.state === "disconnected";
      setOffline(isOffline);
      if (isOffline) {
        setError(null);
        return;
      }
      const res = await aec.kchat.ingestReviews({
        threadId: capturedThread,
        sinceIso: sinceIsoRef.current,
      });
      // After the second await: if a threadId switch landed while
      // we were waiting on the bridge, discard this stale response
      // so old-thread comments don't leak into the new thread's
      // comment list and the sinceIso cursor stays clean.
      if (threadIdRef.current !== capturedThread) return;
      const parsed = parseComments(res.commentsJson);
      if (parsed.length > 0) {
        setComments((prev) => mergeAndSort(prev, parsed));
        const newest = newestTimestamp(parsed);
        if (newest) sinceIsoRef.current = newest;
      }
      setError(null);
    } catch (e) {
      if (threadIdRef.current !== capturedThread) return;
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
    } finally {
      if (threadIdRef.current === capturedThread) {
        setLoading(false);
      }
    }
  }, [props.threadId]);

  useEffect(() => {
    void fetchOnce();
    const id = window.setInterval(() => void fetchOnce(), POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [fetchOnce]);

  if (offline && !props.showWhenOffline) {
    return (
      <aside
        data-testid="kchat-review-panel"
        data-offline="true"
        className="kchat-review-panel kchat-review-panel--offline"
        aria-label="KChat review (offline)"
      >
        <p>KChat is offline. Reviews appear here when KChat Desktop is running.</p>
      </aside>
    );
  }

  return (
    <aside
      data-testid="kchat-review-panel"
      data-offline={offline ? "true" : "false"}
      className="kchat-review-panel"
      aria-label="KChat review comments"
    >
      <header className="kchat-review-panel__header">
        <h3>Reviews · {props.threadId}</h3>
        <button
          type="button"
          data-testid="kchat-review-refresh"
          disabled={loading}
          onClick={() => void fetchOnce()}
        >
          {loading ? "Polling…" : "Refresh"}
        </button>
      </header>
      {error ? (
        <p
          data-testid="kchat-review-error"
          role="status"
          className="kchat-review-panel__error"
        >
          Couldn't reach KChat: {error}. Showing last-known reviews; will retry
          on the next poll.
        </p>
      ) : null}
      {comments.length === 0 ? (
        <p
          data-testid="kchat-review-empty"
          className="kchat-review-panel__empty"
        >
          No reviews on this thread yet.
        </p>
      ) : (
        <ul
          data-testid="kchat-review-list"
          className="kchat-review-panel__list"
        >
          {comments.map((c) => (
            <li key={c.comment_id} className="kchat-review-panel__item">
              <header>
                <strong>{c.author}</strong>
                <time dateTime={c.posted_at}>
                  {new Date(c.posted_at).toLocaleString()}
                </time>
              </header>
              <p>{c.text}</p>
              {c.artifact_id && (
                <p className="kchat-review-panel__artifact">
                  Artifact: <code>{c.artifact_id}</code>
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

function parseComments(json: string): ReviewCommentRow[] {
  try {
    const parsed = JSON.parse(json) as ReviewCommentRow[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function mergeAndSort(
  prev: ReviewCommentRow[],
  next: ReviewCommentRow[],
): ReviewCommentRow[] {
  const byId = new Map<string, ReviewCommentRow>();
  for (const c of prev) byId.set(c.comment_id, c);
  for (const c of next) byId.set(c.comment_id, c);
  return Array.from(byId.values()).sort((a, b) =>
    a.posted_at.localeCompare(b.posted_at),
  );
}

function newestTimestamp(rows: ReviewCommentRow[]): string | null {
  let max: string | null = null;
  for (const r of rows) {
    if (max === null || r.posted_at > max) max = r.posted_at;
  }
  return max;
}
