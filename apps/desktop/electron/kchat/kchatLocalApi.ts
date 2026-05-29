/**
 * AEC Studio ↔ KChat Desktop extension loopback HTTP API.
 *
 * Phase 15 replaces the Phase 12 socket / named-pipe transport
 * (`crates/aec_core::kchat_transport`) with a localhost HTTP
 * surface the `.kcz` extension installed inside KChat Desktop
 * talks to. KChat Desktop's Extension Platform is a JS-only
 * sandbox with no socket listener of its own (verified by
 * reading uneycom/uney-chat-desktop in Phase 14 Task 29), so a
 * loopback API on the AEC side that the extension calls into
 * is the only viable transport. This module is the canonical
 * implementation; `extensions/aec-studio-kchat/src/types.ts`
 * mirrors the wire format.
 *
 * Trust model:
 *
 *   - The server binds to `127.0.0.1` only — every other interface
 *     is rejected by the OS, not by an in-process check. The
 *     unit tests in `kchatLocalApi.test.ts` assert the bound
 *     address.
 *   - The bearer token is generated fresh on every server start
 *     (`crypto.randomBytes(32)` → base64url), persisted only into
 *     `{userData}/aec-kchat-port.json` (mode 0600), and never
 *     leaves the local machine. Token comparison is timing-safe.
 *   - Every request first runs through `validateHostHeader()`,
 *     which accepts only `127.0.0.1[:<port>]` — a DNS-rebinding
 *     defence so an attacker who smuggled a `Host: evil.example`
 *     header onto loopback (e.g. through a misconfigured proxy)
 *     gets a 403 instead of a route.
 *   - Authenticated routes call `requireBearer()`, which performs
 *     a timing-safe comparison against the `Authorization:
 *     Bearer <token>` header. Failed comparisons never update the
 *     `lastExtensionContactMs` heartbeat, so a spamming attacker
 *     cannot keep the Settings card's "KChat Desktop detected"
 *     affordance pinned green.
 *   - Every state-changing route requires
 *     `Content-Type: application/json` and rejects payloads
 *     larger than 64 KiB.
 *
 * The server is otherwise inert: it owns no state machine, no
 * timers, no keep-alive sockets. Requests are dispatched to a
 * `LocalApiHandlers` interface the orchestrator caller supplies,
 * so this module stays decoupled from the rest of the AEC Studio
 * surface and tests can wire in fakes.
 */

import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";
import { AddressInfo } from "node:net";
import { randomBytes, timingSafeEqual } from "node:crypto";
import {
  chmodSync,
  mkdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, resolve as resolvePath } from "node:path";

/** Maximum size of a POST body the server will accept (64 KiB). */
export const MAX_BODY_BYTES = 64 * 1024;

/** Filename of the discovery file under `{userData}`. */
export const PORT_FILE_NAME = "aec-kchat-port.json";

/** Minimum bearer-token length the server will accept from tests. */
export const MIN_TOKEN_LENGTH = 32;

/** Capability strings advertised in `/api/status`. */
export const LOCAL_API_CAPABILITIES: readonly string[] = [
  "status",
  "queued_publishes",
  "publish_to_thread",
  "review_comments",
  "reviews",
];

/**
 * Renderer-facing status the extension probes at activation and
 * polls periodically. `aecStudioVersion` is informational; the
 * extension uses `connected` to decide whether to surface its
 * publish panel.
 */
export interface LocalApiStatus {
  aecStudioVersion: string;
  connected: boolean;
  /** ISO-8601 of the most recent activity (publish, review push). */
  lastEventAt: string | null;
  /** Number of artifact cards currently waiting in the publish queue. */
  queuedPublishCount: number;
  capabilities: readonly string[];
}

/**
 * One artifact card AEC Studio has queued for the extension to
 * publish into a KChat thread. The extension drains the queue
 * via `GET /api/queued-publishes`, posts each card to KChat via
 * `invokeProcedure("kchat.send_message")`, then reports the
 * result via `POST /api/publish-to-thread`.
 */
export interface QueuedPublish {
  cardId: string;
  threadId: string;
  /** Pre-rendered KChat-friendly markdown body. */
  body: string;
  /**
   * Optional ArtifactCard JSON payload (deliver-pack metadata,
   * preview SVG, schedule digest). The extension may attach this
   * to the KChat message as a file. Trimmed to fit under
   * `MAX_BODY_BYTES` minus envelope overhead.
   */
  cardJson: string | null;
  /** ISO-8601 of when the card was enqueued. */
  queuedAt: string;
}

/** Request body of `POST /api/publish-to-thread`. */
export interface PublishToThreadRequest {
  cardId: string;
  threadId: string;
  /** KChat-side message id; non-empty on success. */
  messageId: string;
  /** ISO-8601 when KChat reports the message was posted. */
  postedAt: string;
  /** Optional deeplink the extension built for the posted message. */
  permalink?: string | null;
}

/** Response body of `POST /api/publish-to-thread`. */
export interface PublishToThreadResponse {
  /** Server-side ack id (correlates with `cardId` in audit logs). */
  ackId: string;
  acknowledgedAt: string;
}

/** Request body of `POST /api/review-comments`. */
export interface ReviewCommentsRequest {
  threadId: string;
  /** Comments collected from KChat, in chronological order. */
  comments: readonly ReviewCommentPayload[];
}

export interface ReviewCommentPayload {
  /** KChat post / message id — used to de-duplicate on the AEC side. */
  messageId: string;
  authorId: string;
  authorDisplayName: string;
  bodyMarkdown: string;
  postedAt: string;
  /** Optional KChat permalink for the comment. */
  permalink?: string | null;
}

/** Response body of `POST /api/review-comments`. */
export interface ReviewCommentsResponse {
  threadId: string;
  /** Number of comments accepted (de-duplicated server-side). */
  acceptedCount: number;
  /** Server clock at acceptance. */
  acceptedAt: string;
}

/** Response body of `GET /api/reviews`. */
export interface ReviewsSnapshotResponse {
  /** Per-thread review state, sorted by `lastUpdatedAt` desc. */
  threads: readonly ReviewThreadSummary[];
}

export interface ReviewThreadSummary {
  threadId: string;
  /** Most recently observed comment timestamp. */
  lastUpdatedAt: string | null;
  /** Number of comments AEC Studio has seen for this thread. */
  commentCount: number;
}

/**
 * Wire-level error codes returned in the JSON body of every non-2xx
 * response. Each code is paired with a single canonical HTTP status:
 *
 *   - `unauthorized`        → 401. Bearer token missing/wrong.
 *   - `forbidden`           → 403. Token OK but request rejected on
 *                              policy grounds (currently: Host
 *                              header not loopback). The .kcz
 *                              extension MUST NOT retry — the
 *                              transport is structurally blocked.
 *   - `invalid_request`     → 400. Malformed payload, headers, or
 *                              URL (e.g. wrong Content-Type, empty
 *                              body, schema failure).
 *   - `payload_too_large`   → 413. Body exceeded `MAX_BODY_BYTES`.
 *                              Terminal — chunking will not help
 *                              because the stream has been torn
 *                              down by the time the 413 lands.
 *   - `not_found`           → 404. Unknown route.
 *   - `rate_limited`        → 429. Reserved; not currently emitted.
 *   - `internal_error`      → 500. Uncaught handler exception.
 *   - `aec_unavailable`     → 503. Handler slot has not been wired
 *                              yet (e.g. bridge is still booting).
 *
 * The 401/403 split is load-bearing: a 401 is "refresh the port
 * file and retry"; a 403 is "structurally blocked; do not retry".
 * The 400/413 split is load-bearing: a 413 is terminal, a 400 may
 * be retried with a corrected body. Aligning HTTP status, wire
 * code, and extension retry logic in one canonical mapping keeps
 * the contract coherent across this file, the .kcz extension's
 * type-mirror, and the actual throw sites.
 */
export type LocalApiErrorCode =
  | "unauthorized"
  | "forbidden"
  | "invalid_request"
  | "payload_too_large"
  | "not_found"
  | "rate_limited"
  | "internal_error"
  | "aec_unavailable";

export class LocalApiError extends Error {
  override readonly name = "LocalApiError";
  constructor(
    public readonly status: number,
    public readonly code: LocalApiErrorCode,
    message: string,
  ) {
    super(message);
  }
}

/**
 * Behaviour the orchestrator supplies. Each method is async and
 * may throw `LocalApiError` to surface a typed error envelope.
 */
export interface LocalApiHandlers {
  status(): Promise<LocalApiStatus>;
  fetchQueuedPublishes(): Promise<readonly QueuedPublish[]>;
  publishToThread(
    req: PublishToThreadRequest,
  ): Promise<PublishToThreadResponse>;
  ingestReviewComments(
    req: ReviewCommentsRequest,
  ): Promise<ReviewCommentsResponse>;
  reviewsSnapshot(): Promise<ReviewsSnapshotResponse>;
}

export interface LocalApiServerOptions {
  /** Absolute path of the Electron userData directory. */
  userDataDir: string;
  /** Process id written into the port file (default `process.pid`). */
  pid?: number;
  /** Override server factory (tests). Default `node:http.createServer`. */
  createServerFn?: typeof createServer;
  /** Inject a bearer token (tests). Default = random 32 bytes. */
  tokenForTesting?: string;
  /** Override file-write hook (tests). */
  fsWriter?: PortFileWriter;
  /** Inject a clock (tests). Default `Date.now`. */
  nowMsForTesting?: () => number;
}

export interface PortFileWriter {
  writeAtomic(path: string, contents: string): void;
  unlink(path: string): void;
}

const DEFAULT_FS_WRITER: PortFileWriter = {
  writeAtomic(path, contents) {
    const dir = dirname(path);
    mkdirSync(dir, { recursive: true });
    const tmp = `${path}.${process.pid}.${Date.now()}.tmp`;
    writeFileSync(tmp, contents, { encoding: "utf8", mode: 0o600 });
    try {
      chmodSync(tmp, 0o600);
    } catch {
      // POSIX-only chmod; on Windows the umask covers it.
    }
    renameSync(tmp, path);
  },
  unlink(path) {
    try {
      rmSync(path, { force: true });
    } catch {
      // Best-effort cleanup; the next start overwrites the file.
    }
  },
};

/**
 * Loopback HTTP server. Constructed once per Electron main-process
 * lifetime; `start()` returns the bound port and writes the
 * discovery file. `stop()` removes the file and closes the server.
 */
export class KchatLocalApiServer {
  private readonly handlers: LocalApiHandlers;
  private readonly userDataDir: string;
  private readonly token: string;
  private readonly pid: number;
  private readonly fsWriter: PortFileWriter;
  private readonly createServerFn: typeof createServer;
  private server: Server | null = null;
  private boundPort: number | null = null;
  private portFileAbsPath: string | null = null;
  /**
   * Wall-clock millisecond timestamp (`Date.now()` in production,
   * injectable via `nowMsForTesting`) of the most recent
   * successful authenticated request from the .kcz extension.
   * `null` until the extension has been heard from at least once
   * since this process started. Used by `snapshotForRenderer()`
   * so the Settings card can show the "KChat Desktop detected"
   * affordance without polling.
   *
   * Note: `Date.now()` is NOT monotonic — an NTP step or a manual
   * clock adjustment can make a later sample read smaller than an
   * earlier one. The only consumer is the freshness window in
   * `buildKchatStatusResponse` (`now - last < 30s` ⇒ connected),
   * and that consumer also uses `Date.now()` so the two stay
   * coherent across a step. Switching to `performance.now()` (or
   * `process.hrtime.bigint()`) would be more correct in principle
   * but pointless here because the wire payload exposes
   * `lastExtensionContactAt` as an ISO timestamp, which would
   * still depend on wall-clock for the conversion.
   */
  private lastExtensionContactMs: number | null = null;
  private readonly nowMs: () => number;

  constructor(handlers: LocalApiHandlers, opts: LocalApiServerOptions) {
    this.handlers = handlers;
    this.userDataDir = opts.userDataDir;
    this.pid = opts.pid ?? process.pid;
    this.fsWriter = opts.fsWriter ?? DEFAULT_FS_WRITER;
    this.createServerFn = opts.createServerFn ?? createServer;
    this.nowMs = opts.nowMsForTesting ?? (() => Date.now());
    if (opts.tokenForTesting !== undefined) {
      if (opts.tokenForTesting.length < MIN_TOKEN_LENGTH) {
        throw new Error(
          `tokenForTesting must be at least ${MIN_TOKEN_LENGTH} characters`,
        );
      }
      this.token = opts.tokenForTesting;
    } else {
      this.token = randomBytes(32).toString("base64url");
    }
  }

  /** Absolute path the discovery file will be written to. */
  portFilePath(): string {
    if (this.portFileAbsPath !== null) return this.portFileAbsPath;
    return resolvePath(this.userDataDir, PORT_FILE_NAME);
  }

  /** The bearer token — exposed for tests only. */
  tokenForTests(): string {
    return this.token;
  }

  /** Currently bound port, or `null` while stopped. */
  port(): number | null {
    return this.boundPort;
  }

  /**
   * Renderer-facing projection used by the Settings card. The
   * `lastExtensionContactAt` field is the wall-clock ISO-8601
   * stamp of the heartbeat recorded by `requireBearer()`; the
   * Settings card decides on freshness via its own clock.
   */
  snapshotForRenderer(): {
    apiServerRunning: boolean;
    apiServerPort: number | null;
    portFilePath: string | null;
    lastExtensionContactAt: string | null;
  } {
    return {
      apiServerRunning: this.server !== null,
      apiServerPort: this.boundPort,
      portFilePath: this.portFileAbsPath,
      lastExtensionContactAt:
        this.lastExtensionContactMs === null
          ? null
          : new Date(this.lastExtensionContactMs).toISOString(),
    };
  }

  async start(): Promise<{ port: number; token: string }> {
    if (this.server !== null) {
      throw new Error("KchatLocalApiServer.start called twice");
    }
    const server = this.createServerFn((req, res) => {
      this.dispatch(req, res).catch((err) => {
        // Defence-in-depth wrapper. `dispatch()` already converts
        // every thrown value to `respondError()`; this catch is
        // the last line of defence for a synchronous failure
        // before the try opens, or a `respondError()` edge case.
        // `respondError()` is idempotent via the `headersSent`
        // guard so a second call here when `dispatch()` already
        // responded is a no-op. Non-`LocalApiError` is sanitised
        // to a generic 500 so a handler bug can't leak impl
        // details (paths, stack fragments) over the wire.
        respondError(
          res,
          err instanceof LocalApiError
            ? err
            : new LocalApiError(
                500,
                "internal_error",
                "internal server error",
              ),
          err instanceof LocalApiError ? null : err,
        );
      });
    });
    server.maxConnections = 16;
    server.keepAliveTimeout = 1_500;
    server.requestTimeout = 10_000;
    server.headersTimeout = 5_000;
    await new Promise<void>((resolveFn, rejectFn) => {
      server.once("error", rejectFn);
      // Bind to 127.0.0.1 explicitly. Passing the literal string is
      // load-bearing: `listen(0)` without a host argument resolves
      // to `0.0.0.0` on Linux, which would expose the server on the
      // LAN.
      server.listen({ host: "127.0.0.1", port: 0 }, () => {
        server.removeListener("error", rejectFn);
        resolveFn();
      });
    });
    const address = server.address() as AddressInfo | null;
    if (!address || typeof address === "string") {
      // Defence in depth — practically unreachable because the
      // listen() callback has fired and we requested a TCP bind.
      // If node:net ever surprises us with a null/string address
      // after a successful listen, we still owe the kernel handle
      // a teardown: server.close() releases the event-loop handle
      // that listen() opened.
      server.close();
      throw new Error("KchatLocalApiServer failed to bind");
    }
    if (address.address !== "127.0.0.1") {
      // Defence in depth: listen() already requested 127.0.0.1,
      // but some runtimes resolve "localhost" to a wildcard
      // address in unusual configurations.
      server.close();
      throw new Error(
        `KchatLocalApiServer bound to ${address.address}, expected 127.0.0.1`,
      );
    }
    this.server = server;
    this.boundPort = address.port;
    this.portFileAbsPath = resolvePath(this.userDataDir, PORT_FILE_NAME);
    const portFileContents = JSON.stringify(
      {
        version: 1,
        host: "127.0.0.1",
        port: address.port,
        token: this.token,
        startedAt: new Date().toISOString(),
        pid: this.pid,
      },
      null,
      2,
    );
    try {
      this.fsWriter.writeAtomic(this.portFileAbsPath, portFileContents);
    } catch (err) {
      // If the port-file write fails (disk full, EACCES, EROFS),
      // the HTTP server is already bound. Without a rollback,
      // start() rejects but the listening socket leaks for the
      // process lifetime, holding an event-loop handle that blocks
      // clean shutdown. Close the socket and clear all state so
      // the instance is structurally indistinguishable from one
      // that never called start(). A subsequent start() is then
      // safe to retry. Re-throw the original error so the caller
      // sees the underlying I/O failure.
      const leakedServer = this.server;
      this.server = null;
      this.boundPort = null;
      this.portFileAbsPath = null;
      if (leakedServer !== null) {
        await new Promise<void>((resolveFn) => {
          leakedServer.close(() => resolveFn());
        });
      }
      throw err;
    }
    return { port: address.port, token: this.token };
  }

  async stop(): Promise<void> {
    if (this.portFileAbsPath !== null) {
      this.fsWriter.unlink(this.portFileAbsPath);
      this.portFileAbsPath = null;
    }
    if (this.server === null) return;
    const server = this.server;
    this.server = null;
    this.boundPort = null;
    await new Promise<void>((resolveFn) => {
      server.close(() => resolveFn());
    });
  }

  private async dispatch(
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<void> {
    try {
      if (!req.url) {
        throw new LocalApiError(400, "invalid_request", "missing URL");
      }
      this.validateHostHeader(req);
      const path = (req.url.split("?", 1)[0] ?? "").trim();
      if (req.method === "GET" && path === "/api/status") {
        this.requireBearer(req);
        const value = await this.handlers.status();
        respond(res, 200, value);
        return;
      }
      if (req.method === "GET" && path === "/api/queued-publishes") {
        this.requireBearer(req);
        const value = await this.handlers.fetchQueuedPublishes();
        respond(res, 200, { queued: value });
        return;
      }
      if (req.method === "POST" && path === "/api/publish-to-thread") {
        this.requireBearer(req);
        const body = await readJsonBody<PublishToThreadRequest>(req);
        validatePublishToThreadRequest(body);
        const value = await this.handlers.publishToThread(body);
        respond(res, 200, value);
        return;
      }
      if (req.method === "POST" && path === "/api/review-comments") {
        this.requireBearer(req);
        const body = await readJsonBody<ReviewCommentsRequest>(req);
        validateReviewCommentsRequest(body);
        const value = await this.handlers.ingestReviewComments(body);
        respond(res, 200, value);
        return;
      }
      if (req.method === "GET" && path === "/api/reviews") {
        this.requireBearer(req);
        const value = await this.handlers.reviewsSnapshot();
        respond(res, 200, value);
        return;
      }
      throw new LocalApiError(404, "not_found", "route not found");
    } catch (err) {
      // Non-`LocalApiError` thrown from a handler (e.g. an
      // unexpected TypeError) is sanitised to a generic 500 so
      // the underlying message — which may carry file paths,
      // stack fragments, or other impl details — never escapes
      // past the loopback bearer-token boundary. `LocalApiError`
      // messages are hand-authored throughout this file and are
      // safe to surface as-is.
      const wireErr =
        err instanceof LocalApiError
          ? err
          : new LocalApiError(500, "internal_error", "internal server error");
      const internalErr = err instanceof LocalApiError ? null : err;
      respondError(res, wireErr, internalErr);
    }
  }

  private validateHostHeader(req: IncomingMessage): void {
    const host = req.headers.host;
    if (!host) {
      throw new LocalApiError(
        400,
        "invalid_request",
        "missing Host header",
      );
    }
    // Allow only `127.0.0.1[:<port>]` to defeat DNS-rebinding
    // attacks that swing a public hostname onto loopback. The
    // bound port is not enforced — multiple bound ports across
    // restarts share the same security posture. The wire code
    // is `forbidden` (403), not `unauthorized` (401): the request
    // could be authenticated; the policy says no. Aligning HTTP
    // status and code keeps the extension's retry logic coherent
    // (401 → refresh port file; 403 → do not retry).
    const match = /^127\.0\.0\.1(?::(\d+))?$/.exec(host);
    if (!match) {
      throw new LocalApiError(
        403,
        "forbidden",
        `Host header ${JSON.stringify(host)} is not loopback`,
      );
    }
  }

  private requireBearer(req: IncomingMessage): void {
    const header = req.headers.authorization;
    if (typeof header !== "string" || !header.startsWith("Bearer ")) {
      throw new LocalApiError(401, "unauthorized", "missing bearer token");
    }
    const provided = header.slice("Bearer ".length).trim();
    const expected = this.token;
    const a = Buffer.from(provided, "utf8");
    const b = Buffer.from(expected, "utf8");
    if (a.length !== b.length || !timingSafeEqual(a, b)) {
      throw new LocalApiError(401, "unauthorized", "invalid bearer token");
    }
    // Record the heartbeat AFTER the constant-time comparison so a
    // failed-auth attempt cannot move the timestamp forward.
    this.lastExtensionContactMs = this.nowMs();
  }
}

async function readJsonBody<T>(req: IncomingMessage): Promise<T> {
  const contentType = req.headers["content-type"] ?? "";
  // Accept ONLY `application/json` (optionally `; charset=...`).
  // A laxer `\b` boundary would match `application/json-patch+json`
  // / `application/json-ld`, which `readJsonBody` doesn't actually
  // parse; the stricter check keeps the door closed on stray
  // sibling subtypes a future proxy or caller might emit.
  if (!/^application\/json(?:\s*$|\s*;)/i.test(contentType)) {
    throw new LocalApiError(
      400,
      "invalid_request",
      "Content-Type must be application/json",
    );
  }
  let total = 0;
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    const buf =
      chunk instanceof Buffer ? chunk : Buffer.from(chunk as Uint8Array);
    total += buf.length;
    if (total > MAX_BODY_BYTES) {
      throw new LocalApiError(
        413,
        "payload_too_large",
        "request body exceeds 64 KiB",
      );
    }
    chunks.push(buf);
  }
  if (total === 0) {
    throw new LocalApiError(400, "invalid_request", "empty body");
  }
  const text = Buffer.concat(chunks, total).toString("utf8");
  try {
    return JSON.parse(text) as T;
  } catch (err) {
    throw new LocalApiError(
      400,
      "invalid_request",
      `body is not JSON: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
}

function validatePublishToThreadRequest(body: PublishToThreadRequest): void {
  if (!body || typeof body !== "object") {
    throw new LocalApiError(400, "invalid_request", "body must be an object");
  }
  if (typeof body.cardId !== "string" || body.cardId.length === 0) {
    throw new LocalApiError(400, "invalid_request", "cardId is required");
  }
  if (typeof body.threadId !== "string" || body.threadId.length === 0) {
    throw new LocalApiError(400, "invalid_request", "threadId is required");
  }
  if (typeof body.messageId !== "string" || body.messageId.length === 0) {
    throw new LocalApiError(400, "invalid_request", "messageId is required");
  }
  if (typeof body.postedAt !== "string" || body.postedAt.length === 0) {
    throw new LocalApiError(400, "invalid_request", "postedAt is required");
  }
  if (
    body.permalink !== undefined &&
    body.permalink !== null &&
    (typeof body.permalink !== "string" || body.permalink.length > 2048)
  ) {
    throw new LocalApiError(
      400,
      "invalid_request",
      "permalink must be a string (max 2048 chars) or null when present",
    );
  }
}

function validateReviewCommentsRequest(body: ReviewCommentsRequest): void {
  if (!body || typeof body !== "object") {
    throw new LocalApiError(400, "invalid_request", "body must be an object");
  }
  if (typeof body.threadId !== "string" || body.threadId.length === 0) {
    throw new LocalApiError(400, "invalid_request", "threadId is required");
  }
  if (!Array.isArray(body.comments)) {
    throw new LocalApiError(400, "invalid_request", "comments must be an array");
  }
  if (body.comments.length > 256) {
    throw new LocalApiError(
      400,
      "invalid_request",
      "comments batch must not exceed 256 entries",
    );
  }
  for (let i = 0; i < body.comments.length; i++) {
    const c = body.comments[i] as unknown as Record<string, unknown>;
    if (!c || typeof c !== "object") {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}] must be an object`,
      );
    }
    if (typeof c.messageId !== "string" || c.messageId.length === 0) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].messageId is required`,
      );
    }
    if (typeof c.authorId !== "string" || c.authorId.length === 0) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].authorId is required`,
      );
    }
    if (
      typeof c.authorDisplayName !== "string" ||
      c.authorDisplayName.length === 0 ||
      c.authorDisplayName.length > 256
    ) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].authorDisplayName must be 1-256 chars`,
      );
    }
    if (
      typeof c.bodyMarkdown !== "string" ||
      c.bodyMarkdown.length > 32 * 1024
    ) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].bodyMarkdown must be a string (max 32 KiB)`,
      );
    }
    if (typeof c.postedAt !== "string" || c.postedAt.length === 0) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].postedAt is required`,
      );
    }
    if (
      c.permalink !== undefined &&
      c.permalink !== null &&
      (typeof c.permalink !== "string" || (c.permalink as string).length > 2048)
    ) {
      throw new LocalApiError(
        400,
        "invalid_request",
        `comments[${i}].permalink must be a string (max 2048 chars) or null`,
      );
    }
  }
}

function respond(res: ServerResponse, status: number, body: unknown): void {
  const payload = Buffer.from(JSON.stringify(body), "utf8");
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": String(payload.length),
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  res.end(payload);
}

/**
 * Write a `LocalApiError` to the wire. Idempotent: if headers
 * have already been sent (partial / failed earlier response),
 * this is a no-op rather than the `ERR_HTTP_HEADERS_SENT` throw
 * `respond()` would otherwise emit.
 *
 * `internalErr`, when supplied, is the original non-LocalApiError
 * caught on its way to becoming a generic 500. It is NEVER
 * serialised to the wire body — only logged to stderr so the
 * operator can diagnose the failure from the Electron log
 * pipeline.
 */
function respondError(
  res: ServerResponse,
  err: LocalApiError,
  internalErr: unknown = null,
): void {
  if (internalErr !== null) {
    console.error("[kchatLocalApi] internal handler error:", internalErr);
  }
  if (res.headersSent) {
    if (!res.writableEnded) {
      try {
        res.end();
      } catch {
        // Socket gone; nothing to do.
      }
    }
    return;
  }
  respond(res, err.status, { error: err.message, code: err.code });
}
