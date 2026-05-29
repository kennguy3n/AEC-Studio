/**
 * AEC Studio Publish rightbar view.
 *
 * Renders a small panel inside KChat Desktop that drives the
 * bidirectional bridge between AEC Studio and the active KChat
 * conversation:
 *
 *   1. Polls AEC Studio's loopback API for queued artifact cards
 *      (`GET /api/queued-publishes`).
 *   2. For each queued card, posts it into the requested KChat
 *      thread via `invokeProcedure("kchat.send_message")` and then
 *      acknowledges the result back to AEC Studio via
 *      `POST /api/publish-to-thread`.
 *   3. When the user clicks "Mirror recent comments", queries the
 *      current channel via `invokeProcedure("kchat.query_messages")`
 *      and pushes them to AEC Studio via `POST /api/review-comments`.
 *
 * The component is intentionally minimal — it owns no business
 * logic beyond orchestrating those calls. The orchestration order
 * matters: posting to KChat first and acknowledging second means a
 * lost ack leaves the queue entry in AEC Studio so the next
 * extension activation will replay it; the AEC-side queue is
 * de-duplicated by `cardId` so the replay is idempotent.
 */
import {
  useCallback,
  useEffect,
  useState,
  type ReactElement,
} from "react";

import {
  AecStudioLocalApiClient,
  AecStudioLocalApiHttpError,
  AecStudioLocalApiUnavailableError,
} from "../client";
import {
  HostProcedureError,
  queryMessages,
  sendMessage,
} from "../host";
import type {
  AecStudioLocalApiStatus,
  QueuedPublish,
  ReviewCommentPayload,
  ReviewsSnapshotResponse,
} from "../types";

export interface PublishPanelHostBridge {
  /** Read the discovery file managed by AEC Studio. */
  readPortFile(): Promise<string | null>;
  /** Open an `aecstudio://` URL via the host's secure-shell helper. */
  openExternal(url: string): Promise<void>;
  /** The current channel id, when the user is viewing a conversation. */
  currentChannelId: string | null;
  currentChannelName: string | null;
  currentTeamId: string | null;
}

export interface PublishPanelProps {
  bridge: PublishPanelHostBridge;
  /** Test seam — inject a pre-built client. */
  client?: AecStudioLocalApiClient;
}

interface PanelData {
  status: AecStudioLocalApiStatus;
  queued: readonly QueuedPublish[];
  reviews: ReviewsSnapshotResponse;
}

type LoadState =
  | { kind: "loading" }
  | { kind: "unavailable"; reason: string }
  | { kind: "ready"; data: PanelData };

export function AecStudioPublishPanel(
  props: PublishPanelProps,
): ReactElement {
  const { bridge } = props;
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [pendingPublish, setPendingPublish] = useState(false);
  const [pendingMirror, setPendingMirror] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setState({ kind: "loading" });
    try {
      const client =
        props.client ?? (await buildClient(bridge.readPortFile));
      const [status, queued, reviews] = await Promise.all([
        client.status(),
        client.fetchQueuedPublishes(),
        client.reviewsSnapshot(),
      ]);
      setState({ kind: "ready", data: { status, queued, reviews } });
      setLastError(null);
    } catch (err) {
      setState({
        kind: "unavailable",
        reason: describeError(err),
      });
    }
  }, [bridge.readPortFile, props.client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onPublishNext = useCallback(async () => {
    if (state.kind !== "ready" || state.data.queued.length === 0) return;
    const first = state.data.queued[0];
    if (!first) return;
    setPendingPublish(true);
    try {
      const client =
        props.client ?? (await buildClient(bridge.readPortFile));
      // Step 1 — Post the artifact card to KChat. The host owns the
      // schema for `kchat.send_message`; if the host rejects the call
      // we surface the error verbatim so the user can see whether
      // it's a consent failure (CONSENT_REQUIRED), rate limit, or a
      // structural issue (INVALID_REQUEST).
      const sent = await sendMessage({
        channelId: first.threadId,
        bodyMarkdown: first.body,
        ...(first.cardJson
          ? {
              attachment: {
                filename: `${first.cardId}.json`,
                contentType: "application/json",
                dataBase64: encodeBase64(first.cardJson),
              },
            }
          : {}),
      });
      // Step 2 — Acknowledge back to AEC Studio so the queue can
      // drop the entry. If this fails, AEC Studio will replay the
      // same `cardId` on the next refresh; KChat-side duplicate
      // detection on `messageId` keeps that idempotent.
      await client.acknowledgePublish({
        cardId: first.cardId,
        threadId: first.threadId,
        messageId: sent.messageId,
        postedAt: sent.postedAt,
        permalink: sent.permalink ?? null,
      });
      setLastError(null);
      await refresh();
    } catch (err) {
      setLastError(describeError(err));
      await refresh();
    } finally {
      setPendingPublish(false);
    }
  }, [bridge.readPortFile, props.client, refresh, state]);

  const onMirrorRecent = useCallback(async () => {
    if (!bridge.currentChannelId) return;
    setPendingMirror(true);
    try {
      const client =
        props.client ?? (await buildClient(bridge.readPortFile));
      const sinceIso = lastSeenForThread(
        state.kind === "ready" ? state.data.reviews : null,
        bridge.currentChannelId,
      );
      // Query recent KChat messages. The host enforces consent +
      // capability; we only see the post-redaction DTO.
      const fetched = await queryMessages({
        channelId: bridge.currentChannelId,
        ...(sinceIso ? { since: sinceIso } : {}),
        limit: 50,
      });
      const comments: ReviewCommentPayload[] = fetched.messages.map(
        (m) => ({
          messageId: m.id,
          authorId: m.authorId,
          authorDisplayName: m.authorDisplayName,
          bodyMarkdown: m.bodyMarkdown,
          postedAt: m.postedAt,
          permalink: m.permalink ?? null,
        }),
      );
      await client.pushReviewComments({
        threadId: bridge.currentChannelId,
        comments,
      });
      setLastError(null);
      await refresh();
    } catch (err) {
      setLastError(describeError(err));
      await refresh();
    } finally {
      setPendingMirror(false);
    }
  }, [
    bridge.currentChannelId,
    bridge.readPortFile,
    props.client,
    refresh,
    state,
  ]);

  return (
    <section
      className="aecstudio-publish-panel"
      aria-label="AEC Studio Publish"
    >
      <header className="aecstudio-publish-panel__header">
        <h2>AEC Studio Publish</h2>
        <button
          type="button"
          onClick={() => void refresh()}
          aria-label="Refresh AEC Studio status"
          disabled={state.kind === "loading"}
        >
          Refresh
        </button>
      </header>
      {state.kind === "loading" && (
        <p className="aecstudio-publish-panel__loading">
          Connecting to AEC Studio…
        </p>
      )}
      {state.kind === "unavailable" && (
        <UnavailableBlock reason={state.reason} onRetry={refresh} />
      )}
      {state.kind === "ready" && (
        <ReadyBlock
          data={state.data}
          currentChannelId={bridge.currentChannelId}
          currentChannelName={bridge.currentChannelName}
          pendingPublish={pendingPublish}
          pendingMirror={pendingMirror}
          onPublishNext={onPublishNext}
          onMirrorRecent={onMirrorRecent}
          lastError={lastError}
        />
      )}
    </section>
  );
}

function UnavailableBlock(props: {
  reason: string;
  onRetry: () => void;
}): ReactElement {
  return (
    <div
      role="status"
      className="aecstudio-publish-panel__unavailable"
    >
      <p>AEC Studio is not running on this machine.</p>
      <p className="aecstudio-publish-panel__hint">{props.reason}</p>
      <button type="button" onClick={() => void props.onRetry()}>
        Retry
      </button>
    </div>
  );
}

function ReadyBlock(props: {
  data: PanelData;
  currentChannelId: string | null;
  currentChannelName: string | null;
  pendingPublish: boolean;
  pendingMirror: boolean;
  onPublishNext: () => void;
  onMirrorRecent: () => void;
  lastError: string | null;
}): ReactElement {
  const { data } = props;
  const next = data.queued[0] ?? null;
  return (
    <>
      <div
        className="aecstudio-publish-panel__status"
        aria-label="AEC Studio connection status"
      >
        <span
          className={
            data.status.connected
              ? "aecstudio-publish-panel__dot aecstudio-publish-panel__dot--ok"
              : "aecstudio-publish-panel__dot aecstudio-publish-panel__dot--idle"
          }
          aria-hidden
        />
        <span>
          {data.status.connected
            ? `AEC Studio ${data.status.aecStudioVersion} connected`
            : `AEC Studio ${data.status.aecStudioVersion} (disconnected)`}
        </span>
      </div>
      <p className="aecstudio-publish-panel__queue-summary">
        {data.queued.length === 0
          ? "Publish queue is empty."
          : `${data.queued.length} card${data.queued.length === 1 ? "" : "s"} queued.`}
      </p>
      {next && (
        <button
          type="button"
          className="aecstudio-publish-panel__publish"
          disabled={props.pendingPublish}
          onClick={() => props.onPublishNext()}
        >
          {props.pendingPublish
            ? "Publishing…"
            : `Publish next → thread ${next.threadId}`}
        </button>
      )}
      {props.currentChannelId && (
        <button
          type="button"
          className="aecstudio-publish-panel__mirror"
          disabled={props.pendingMirror}
          onClick={() => props.onMirrorRecent()}
        >
          {props.pendingMirror
            ? "Mirroring…"
            : `Mirror recent comments from #${props.currentChannelName ?? props.currentChannelId}`}
        </button>
      )}
      <ul
        className="aecstudio-publish-panel__reviews"
        aria-label="AEC Studio review-thread state"
      >
        {data.reviews.threads.length === 0 && (
          <li className="aecstudio-publish-panel__empty">
            No threads mirrored yet.
          </li>
        )}
        {data.reviews.threads.map((row) => (
          <li
            key={row.threadId}
            className="aecstudio-publish-panel__review-row"
          >
            <span className="aecstudio-publish-panel__review-thread">
              {row.threadId}
            </span>
            <span className="aecstudio-publish-panel__review-count">
              {row.commentCount} comment{row.commentCount === 1 ? "" : "s"}
            </span>
          </li>
        ))}
      </ul>
      {props.lastError && (
        <p
          role="alert"
          className="aecstudio-publish-panel__last-error"
        >
          {props.lastError}
        </p>
      )}
    </>
  );
}

async function buildClient(
  read: () => Promise<string | null>,
): Promise<AecStudioLocalApiClient> {
  const { readPortFile } = await import("../portFile");
  const result = await readPortFile({ read });
  if (!result.ok) {
    throw new AecStudioLocalApiUnavailableError(
      `AEC Studio port file is ${result.reason}${
        result.detail ? `: ${result.detail}` : ""
      }.`,
    );
  }
  return new AecStudioLocalApiClient({ portFile: result.value });
}

function lastSeenForThread(
  reviews: ReviewsSnapshotResponse | null,
  threadId: string,
): string | null {
  if (!reviews) return null;
  const row = reviews.threads.find((t) => t.threadId === threadId);
  return row?.lastUpdatedAt ?? null;
}

/**
 * Base64-encode a UTF-8 string. We avoid `Buffer` (Node-only) so
 * the bundle runs unchanged inside the KChat Desktop sandbox where
 * Node globals are stripped.
 */
function encodeBase64(input: string): string {
  // `btoa` is part of the host environment's window globals. We
  // map UTF-8 → binary-safe Latin-1 first because `btoa` rejects
  // bytes outside the 0..255 range — the standard recipe in the
  // WHATWG spec is to URI-encode and then peel off the percent
  // escapes back to bytes.
  const utf8 = encodeURIComponent(input).replace(
    /%([0-9A-F]{2})/g,
    (_, hex: string) => String.fromCharCode(parseInt(hex, 16)),
  );
  return btoa(utf8);
}

function describeError(err: unknown): string {
  if (err instanceof AecStudioLocalApiHttpError) {
    return `AEC Studio returned ${err.status} (${err.body.code}).`;
  }
  if (err instanceof AecStudioLocalApiUnavailableError) {
    return err.message;
  }
  if (err instanceof HostProcedureError) {
    return `KChat host refused ${err.procedureId} (${err.kind}).`;
  }
  return err instanceof Error ? err.message : String(err);
}
