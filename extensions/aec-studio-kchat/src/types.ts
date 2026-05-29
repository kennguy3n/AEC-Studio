/**
 * Shared type declarations for the AEC Studio KChat extension.
 *
 * These types describe the wire format AEC Studio's loopback API
 * server (`apps/desktop/electron/kchat/kchatLocalApi.ts`) exchanges
 * with the extension. AEC Studio owns the canonical schema; this
 * file is a hand-rolled mirror because the extension has no
 * build-time dependency on AEC Studio's main-process bundle. The
 * two sides MUST stay byte-for-byte compatible — the field names,
 * the value casings, and the discriminated-union members on
 * `code` below all match the Electron-side definitions verbatim.
 */

/** Response body of `GET /api/status`. */
export interface AecStudioLocalApiStatus {
  /** AEC Studio app version (informational; ext uses `connected`). */
  aecStudioVersion: string;
  /** Whether the loopback API is currently accepting publishes. */
  connected: boolean;
  /** ISO-8601 of the most recent activity (publish, review push). */
  lastEventAt: string | null;
  /** Number of artifact cards currently waiting to be published. */
  queuedPublishCount: number;
  /** Advertised capabilities — e.g. `status`, `publish`, `reviews`. */
  capabilities: readonly string[];
}

/**
 * One artifact card AEC Studio has queued for the extension to
 * publish into a KChat thread. Returned from `GET /api/queued-publishes`.
 * After the extension successfully posts the card via
 * `invokeProcedure("kchat.send_message")`, it MUST acknowledge it
 * via `POST /api/publish-to-thread` so the AEC Studio queue can
 * drop it.
 */
export interface QueuedPublish {
  /** Stable id assigned by AEC Studio when the card was enqueued. */
  cardId: string;
  /** KChat thread the card should be posted into. */
  threadId: string;
  /** Pre-rendered KChat-friendly markdown body. */
  body: string;
  /**
   * Optional ArtifactCard JSON payload (deliver-pack metadata,
   * preview SVG, schedule digest). The extension may attach this
   * to the KChat message as a file. AEC Studio trims this to fit
   * under `MAX_BODY_BYTES` (64 KiB) minus envelope overhead.
   */
  cardJson: string | null;
  /** ISO-8601 of when the card was enqueued by AEC Studio. */
  queuedAt: string;
}

/** Response body of `GET /api/queued-publishes`. */
export interface QueuedPublishesResponse {
  queued: readonly QueuedPublish[];
}

/** Request body of `POST /api/publish-to-thread`. */
export interface PublishToThreadRequest {
  /** Echoes the `cardId` from the queue entry being acknowledged. */
  cardId: string;
  /** Echoes the `threadId` from the queue entry. */
  threadId: string;
  /** KChat-side message id; non-empty on success. */
  messageId: string;
  /** ISO-8601 when KChat reports the message was posted. */
  postedAt: string;
  /** Optional KChat permalink the extension built for the post. */
  permalink?: string | null;
}

/** Response body of `POST /api/publish-to-thread`. */
export interface PublishToThreadResponse {
  /** Server-side ack id (correlates with `cardId` in audit logs). */
  ackId: string;
  /** ISO-8601 of acknowledgement on the AEC Studio side. */
  acknowledgedAt: string;
}

/**
 * One review comment the extension scraped from KChat (via
 * `kchat.query_messages`) and is pushing back to AEC Studio so the
 * Deliver page's review panel can surface it.
 */
export interface ReviewCommentPayload {
  /** KChat post/message id — used to de-duplicate on the AEC side. */
  messageId: string;
  /** KChat user id of the comment author. */
  authorId: string;
  /** Display name of the author. */
  authorDisplayName: string;
  /** Markdown body of the comment, untouched. */
  bodyMarkdown: string;
  /** ISO-8601 when KChat reports the comment was posted. */
  postedAt: string;
  /** Optional KChat permalink for the comment. */
  permalink?: string | null;
}

/** Request body of `POST /api/review-comments`. */
export interface ReviewCommentsRequest {
  /** Thread the comments belong to. */
  threadId: string;
  /** Comments in chronological order, with no duplicates. */
  comments: readonly ReviewCommentPayload[];
}

/** Response body of `POST /api/review-comments`. */
export interface ReviewCommentsResponse {
  threadId: string;
  /** Number of comments accepted (de-duplicated server-side). */
  acceptedCount: number;
  /** Server clock at acceptance. */
  acceptedAt: string;
}

/** One per-thread summary returned by `GET /api/reviews`. */
export interface ReviewThreadSummary {
  threadId: string;
  /** Most recently observed comment timestamp. */
  lastUpdatedAt: string | null;
  /** Number of comments AEC Studio has seen for this thread. */
  commentCount: number;
}

/** Response body of `GET /api/reviews`. */
export interface ReviewsSnapshotResponse {
  /** Per-thread review state, sorted by `lastUpdatedAt` desc. */
  threads: readonly ReviewThreadSummary[];
}

/**
 * Standard error envelope for non-2xx responses.
 *
 * Wire codes (mirror of the canonical `LocalApiErrorCode` in
 * `apps/desktop/electron/kchat/kchatLocalApi.ts` — both must stay
 * in sync):
 *
 *   - `unauthorized`        → 401. Bearer token missing or wrong.
 *                              The extension should re-read the
 *                              port file and retry, since an AEC
 *                              Studio restart rotates the token.
 *   - `forbidden`           → 403. Token is fine, but the request
 *                              is rejected on a separate policy
 *                              ground (currently: non-loopback
 *                              `Host` header). MUST NOT retry —
 *                              the request is structurally
 *                              blocked, not stale.
 *   - `invalid_request`     → 400. Malformed payload, headers, or
 *                              URL (wrong Content-Type, empty
 *                              body, schema failure). The body fit
 *                              in the size budget; what's inside
 *                              is the problem.
 *   - `payload_too_large`   → 413. The request body exceeded the
 *                              server's `MAX_BODY_BYTES` (64 KiB).
 *                              Treat as terminal — chunking the
 *                              request will not help, because the
 *                              server has already torn down the
 *                              read stream by the time the 413
 *                              lands.
 *   - `not_found`           → 404. Unknown route or resource.
 *   - `rate_limited`        → 429. (Reserved; not currently
 *                              emitted by AEC Studio.)
 *   - `internal_error`      → 500. Uncaught handler exception.
 *   - `aec_unavailable`     → 503. Handler slot not wired yet
 *                              (bridge still booting).
 */
export interface AecStudioLocalApiError {
  error: string;
  /** Machine-readable code so the extension can branch UX on it. */
  code:
    | "unauthorized"
    | "forbidden"
    | "invalid_request"
    | "payload_too_large"
    | "not_found"
    | "rate_limited"
    | "internal_error"
    | "aec_unavailable";
}

/**
 * Shape of `{userData}/aec-kchat-port.json`, the discovery file
 * AEC Studio writes when its loopback API server starts. The
 * extension reads it on activation to learn which port + token to
 * use.
 *
 * AEC Studio writes the file with mode 0600 via atomic rename; the
 * extension should treat it as read-only.
 */
export interface AecStudioPortFileV1 {
  version: 1;
  host: "127.0.0.1";
  port: number;
  token: string;
  startedAt: string;
  pid: number;
}

/**
 * Minimum length of the bearer token AEC Studio mints (32
 * characters of base64url — the same threshold `kchatLocalApi.ts`
 * enforces in `MIN_TOKEN_LENGTH`). Used by `validatePortFile` to
 * refuse short tokens, since a too-short token is almost certainly
 * a corrupt port file rather than a legitimate AEC Studio token.
 */
export const MIN_TOKEN_LENGTH = 32;
