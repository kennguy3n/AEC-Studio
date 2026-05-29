/**
 * Process-wide state owned by the Electron main process for the
 * Phase 15 KChat integration.
 *
 * Lifecycle:
 *
 *   1. `initialiseKchat(userDataDir)` is called once from
 *      `main.ts` inside the `app.whenReady()` callback. It
 *      starts the loopback HTTP server, sets up the deeplink
 *      bridge, and stashes both into the module-level singleton
 *      slot. Idempotent: a second call is a no-op.
 *   2. `getKchatLocalApiSnapshot()` / `getKchatDeeplinkBridge()`
 *      / `enqueuePublish(...)` / `getReviewsSnapshot()` /
 *      `consumeQueuedPublishes()` are the surface other Electron
 *      modules (`ipc.ts`, `main.ts`, the deeplink consumer) talk
 *      to. None of them spin up new servers or sockets.
 *   3. `shutdownKchat()` is called from `app.on("will-quit", …)`.
 *      It tears down the server, deletes the discovery file, and
 *      clears the singleton.
 *
 * The publish queue is in-process and bounded. AEC Studio's
 * renderer enqueues an artifact card via the
 * `kchat:enqueuePublish` IPC channel (wired in `ipc.ts`); the
 * `.kcz` extension running inside KChat Desktop drains the queue
 * via `GET /api/queued-publishes`, posts each card to KChat via
 * `invokeProcedure("kchat.send_message")`, then reports the
 * result via `POST /api/publish-to-thread`. A successful
 * acknowledgement removes the card from the queue and appends
 * it to the per-thread review snapshot so the renderer can
 * surface "posted at HH:MM" without a re-fetch.
 *
 * The queue cap (`MAX_QUEUED_PUBLISHES = 32`) is the same order
 * of magnitude as the typical deliver workflow (one pack →
 * 4-6 artifact shares). Hitting the cap means the extension has
 * not drained the queue for a long time (KChat Desktop offline,
 * extension disabled), so the oldest entry is evicted with a
 * structured log line — silently dropping would hide a bug.
 */

import { app as electronApp } from "electron";
import { dirname } from "node:path";
import { mkdirSync } from "node:fs";

import {
  KchatLocalApiServer,
  type LocalApiHandlers,
  type LocalApiStatus,
  type PublishToThreadRequest,
  type PublishToThreadResponse,
  type QueuedPublish,
  type ReviewCommentPayload,
  type ReviewCommentsRequest,
  type ReviewCommentsResponse,
  type ReviewThreadSummary,
  type ReviewsSnapshotResponse,
} from "./kchatLocalApi";
import {
  DeeplinkBridge,
  attachAppEvents,
  type DeeplinkRoute,
} from "./kchatDeeplinkBridge";

/** Maximum artifact cards retained in the publish queue. */
export const MAX_QUEUED_PUBLISHES = 32;

/** Maximum review comments retained per thread (oldest evicted). */
export const MAX_REVIEW_COMMENTS_PER_THREAD = 512;

/** Snapshot returned to the renderer Settings card. */
export interface KchatRendererSnapshot {
  apiServerRunning: boolean;
  apiServerPort: number | null;
  portFilePath: string | null;
  lastExtensionContactAt: string | null;
  queuedPublishCount: number;
  reviewThreadCount: number;
}

/**
 * In-memory model for one comment AEC Studio learned about from
 * the extension. Identical to `ReviewCommentPayload` plus a
 * server-side `ingestedAt`.
 */
export interface StoredReviewComment extends ReviewCommentPayload {
  ingestedAt: string;
}

interface ReviewThreadState {
  threadId: string;
  comments: StoredReviewComment[];
  /** `messageId` set for O(1) duplicate detection. */
  seenIds: Set<string>;
  lastUpdatedAt: string | null;
}

interface KchatProcessState {
  server: KchatLocalApiServer;
  deeplink: DeeplinkBridge;
  detachAppEvents: () => void;
  publishQueue: QueuedPublish[];
  reviewThreads: Map<string, ReviewThreadState>;
  /** ISO-8601 of most-recent activity recorded by any handler. */
  lastEventAt: string | null;
}

let state: KchatProcessState | null = null;

/**
 * Best-effort version string surfaced in `/api/status`. The
 * Electron main process can read it from `electronApp.getVersion()`
 * after `whenReady`, but tests inject `"0.0.0-test"` so the API
 * surface is exercised without booting Electron.
 */
function aecStudioVersion(): string {
  try {
    return electronApp.getVersion();
  } catch {
    return "0.0.0";
  }
}

function nowIso(): string {
  return new Date().toISOString();
}

/**
 * Wire the loopback API + deeplink bridge for this process. Safe
 * to call multiple times — only the first call has effect. Tests
 * may pass an override `userDataDir` to keep state per-test.
 */
export async function initialiseKchat(
  userDataDir?: string,
): Promise<KchatProcessState> {
  if (state !== null) return state;
  const resolvedUserData =
    userDataDir ?? electronApp.getPath("userData");
  // Make sure the userData dir exists before the port-file writer
  // tries to atomic-rename into it. Electron does this on its own
  // path, but the tests inject custom paths.
  mkdirSync(dirname(resolvedUserData), { recursive: true });
  mkdirSync(resolvedUserData, { recursive: true });

  const handlers = buildHandlers();
  const server = new KchatLocalApiServer(handlers, {
    userDataDir: resolvedUserData,
  });
  await server.start();

  const deeplink = new DeeplinkBridge({
    onParseFailure: (raw, reason, detail) => {
      console.warn(
        `[kchatAppState] dropped malformed deeplink ${JSON.stringify(raw)}: ${reason}${
          detail !== undefined ? ` (${detail})` : ""
        }`,
      );
    },
  });
  // NOTE: the `aecstudio://` scheme is claimed BEFORE `whenReady`
  // in `main.ts` so the very first `open-url` event on macOS isn't
  // racy. We deliberately do not re-register here — repeating the
  // `setAsDefaultProtocolClient` call would be a no-op on macOS /
  // Linux and (silently) succeed on Windows, but the duplicate
  // hides the lifecycle invariant from readers.
  const detachAppEvents = attachAppEvents(deeplink, electronApp);

  state = {
    server,
    deeplink,
    detachAppEvents,
    publishQueue: [],
    reviewThreads: new Map(),
    lastEventAt: null,
  };
  return state;
}

/**
 * Build the `LocalApiHandlers` bound to the singleton state. The
 * handlers are constructed eagerly so the closure captures the
 * eventual `state` slot via the module-level binding rather than
 * a stale snapshot.
 */
function buildHandlers(): LocalApiHandlers {
  return {
    async status(): Promise<LocalApiStatus> {
      const s = state;
      return {
        aecStudioVersion: aecStudioVersion(),
        connected: s !== null && s.server.port() !== null,
        lastEventAt: s?.lastEventAt ?? null,
        queuedPublishCount: s?.publishQueue.length ?? 0,
        capabilities: [
          "status",
          "queued_publishes",
          "publish_to_thread",
          "review_comments",
          "reviews",
        ],
      };
    },
    async fetchQueuedPublishes(): Promise<readonly QueuedPublish[]> {
      const s = state;
      if (s === null) return [];
      // Return a defensive snapshot — the extension owns the
      // ordering, but it should not mutate the queue out from
      // under us. The queue is drained only on
      // `publishToThread()` ack.
      return s.publishQueue.map((item) => ({ ...item }));
    },
    async publishToThread(
      req: PublishToThreadRequest,
    ): Promise<PublishToThreadResponse> {
      const s = state;
      if (s === null) {
        return {
          ackId: req.cardId,
          acknowledgedAt: nowIso(),
        };
      }
      const idx = s.publishQueue.findIndex(
        (item) =>
          item.cardId === req.cardId && item.threadId === req.threadId,
      );
      if (idx >= 0) {
        s.publishQueue.splice(idx, 1);
      }
      // Record the publish event as a synthetic "system" review
      // comment so the Deliver page's review panel surfaces it
      // alongside KChat replies. The extension can later push
      // the real reply chain via /api/review-comments.
      ingestComments(s, req.threadId, [
        {
          messageId: req.messageId,
          authorId: "aec-studio",
          authorDisplayName: "AEC Studio (published)",
          bodyMarkdown: `Artifact \`${req.cardId}\` posted to thread.`,
          postedAt: req.postedAt,
          permalink: req.permalink ?? null,
        },
      ]);
      s.lastEventAt = nowIso();
      return {
        ackId: req.cardId,
        acknowledgedAt: s.lastEventAt,
      };
    },
    async ingestReviewComments(
      req: ReviewCommentsRequest,
    ): Promise<ReviewCommentsResponse> {
      const s = state;
      const acceptedAt = nowIso();
      if (s === null) {
        return {
          threadId: req.threadId,
          acceptedCount: 0,
          acceptedAt,
        };
      }
      const acceptedCount = ingestComments(
        s,
        req.threadId,
        req.comments,
      );
      s.lastEventAt = acceptedAt;
      return {
        threadId: req.threadId,
        acceptedCount,
        acceptedAt,
      };
    },
    async reviewsSnapshot(): Promise<ReviewsSnapshotResponse> {
      const s = state;
      if (s === null) return { threads: [] };
      const threads: ReviewThreadSummary[] = [];
      for (const t of s.reviewThreads.values()) {
        threads.push({
          threadId: t.threadId,
          lastUpdatedAt: t.lastUpdatedAt,
          commentCount: t.comments.length,
        });
      }
      threads.sort((a, b) => {
        const la = a.lastUpdatedAt ?? "";
        const lb = b.lastUpdatedAt ?? "";
        if (la === lb) return a.threadId.localeCompare(b.threadId);
        return lb.localeCompare(la);
      });
      return { threads };
    },
  };
}

function ingestComments(
  s: KchatProcessState,
  threadId: string,
  comments: readonly ReviewCommentPayload[],
): number {
  let thread = s.reviewThreads.get(threadId);
  if (thread === undefined) {
    thread = {
      threadId,
      comments: [],
      seenIds: new Set(),
      lastUpdatedAt: null,
    };
    s.reviewThreads.set(threadId, thread);
  }
  let accepted = 0;
  const ingestedAt = nowIso();
  for (const c of comments) {
    if (thread.seenIds.has(c.messageId)) continue;
    thread.seenIds.add(c.messageId);
    thread.comments.push({ ...c, ingestedAt });
    accepted += 1;
    if (thread.lastUpdatedAt === null || c.postedAt > thread.lastUpdatedAt) {
      thread.lastUpdatedAt = c.postedAt;
    }
  }
  // Bound memory — drop oldest comments past the per-thread cap.
  // seenIds is intentionally NOT pruned alongside the dropped comments:
  // the dedup contract with the extension client is "we have seen this
  // messageId, do not resend it." If we forgot evicted ids the same
  // comment would be re-ingested on the next poll, defeating the cap.
  if (thread.comments.length > MAX_REVIEW_COMMENTS_PER_THREAD) {
    thread.comments.sort((a, b) => a.postedAt.localeCompare(b.postedAt));
    const overflow =
      thread.comments.length - MAX_REVIEW_COMMENTS_PER_THREAD;
    thread.comments.splice(0, overflow);
  }
  return accepted;
}

/**
 * Enqueue an artifact card for the extension to drain. Returns
 * the cardId actually queued so the caller can correlate the
 * eventual `publish-to-thread` ack.
 *
 * If the queue is full, the oldest card is evicted with a
 * structured warning. Returning silently would hide a real
 * problem (extension offline, KChat Desktop unreachable) and
 * cause the renderer to think the card was posted.
 */
export function enqueuePublish(card: {
  cardId: string;
  threadId: string;
  body: string;
  cardJson?: string | null;
}): QueuedPublish {
  if (state === null) {
    throw new Error(
      "kchatAppState: initialiseKchat() must complete before enqueuePublish",
    );
  }
  const queued: QueuedPublish = {
    cardId: card.cardId,
    threadId: card.threadId,
    body: card.body,
    cardJson: card.cardJson ?? null,
    queuedAt: nowIso(),
  };
  state.publishQueue.push(queued);
  while (state.publishQueue.length > MAX_QUEUED_PUBLISHES) {
    const dropped = state.publishQueue.shift();
    if (dropped === undefined) break;
    console.warn(
      `[kchatAppState] publish queue full; evicting oldest card ${dropped.cardId}`,
    );
  }
  state.lastEventAt = queued.queuedAt;
  return queued;
}

/** Renderer-facing snapshot used by the Settings card. */
export function getKchatRendererSnapshot(): KchatRendererSnapshot {
  if (state === null) {
    return {
      apiServerRunning: false,
      apiServerPort: null,
      portFilePath: null,
      lastExtensionContactAt: null,
      queuedPublishCount: 0,
      reviewThreadCount: 0,
    };
  }
  const base = state.server.snapshotForRenderer();
  return {
    ...base,
    queuedPublishCount: state.publishQueue.length,
    reviewThreadCount: state.reviewThreads.size,
  };
}

/** Per-thread review comments for the renderer's Deliver page. */
export function getReviewCommentsForThread(
  threadId: string,
  sinceIso?: string | null,
): readonly StoredReviewComment[] {
  if (state === null) return [];
  const t = state.reviewThreads.get(threadId);
  if (t === undefined) return [];
  if (sinceIso === undefined || sinceIso === null || sinceIso.length === 0) {
    return t.comments.map((c) => ({ ...c }));
  }
  return t.comments
    .filter((c) => c.postedAt > sinceIso)
    .map((c) => ({ ...c }));
}

/** Accessor for the deeplink bridge so `main.ts` can wire IPC. */
export function getKchatDeeplinkBridge(): DeeplinkBridge | null {
  return state?.deeplink ?? null;
}

/** Drain the queue from outside the HTTP handler path (tests / IPC). */
export function consumeQueuedPublishes(): readonly QueuedPublish[] {
  if (state === null) return [];
  const drained = state.publishQueue.splice(0, state.publishQueue.length);
  return drained;
}

/** Expose the deeplink dispatch so renderer-initiated routes work too. */
export function dispatchDeeplink(route: DeeplinkRoute): void {
  state?.deeplink.dispatch(route);
}

/** Tear down the server + listeners on app quit. */
export async function shutdownKchat(): Promise<void> {
  if (state === null) return;
  const s = state;
  state = null;
  s.detachAppEvents();
  try {
    await s.server.stop();
  } catch (err) {
    console.error("[kchatAppState] server.stop() failed:", err);
  }
}

/** Reset for unit tests. */
export function __resetKchatStateForTesting(): void {
  state = null;
}
