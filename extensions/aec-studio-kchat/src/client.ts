/**
 * Typed HTTP client the extension uses to talk to AEC Studio's
 * loopback API. The implementation is intentionally framework-free
 * so it runs unchanged inside KChat Desktop's extension sandbox
 * (browser-style `fetch`).
 *
 * All requests:
 *   1. Target `http://127.0.0.1:<port>` discovered from the port
 *      file (`aec-kchat-port.json`).
 *   2. Carry the bearer token also discovered from the port file.
 *   3. Time out aggressively (5 s) — AEC Studio runs on the same
 *      machine and a slow response means it's hung / crashed, in
 *      which case the extension should surface "AEC Studio
 *      unavailable" rather than stall its rightbar view.
 *   4. Reject any redirect — the API is loopback-bound, so a 30x
 *      response would indicate a bug or a hijack attempt.
 *   5. Refuse short tokens client-side as defence in depth, since
 *      a test seam that wires the client directly bypasses the
 *      port-file reader.
 */
import { MIN_TOKEN_LENGTH } from "./types";
import type {
  AecStudioLocalApiError,
  AecStudioLocalApiStatus,
  AecStudioPortFileV1,
  PublishToThreadRequest,
  PublishToThreadResponse,
  QueuedPublish,
  QueuedPublishesResponse,
  ReviewCommentsRequest,
  ReviewCommentsResponse,
  ReviewsSnapshotResponse,
} from "./types";

export class AecStudioLocalApiUnavailableError extends Error {
  override readonly name = "AecStudioLocalApiUnavailableError";
  constructor(message: string) {
    super(message);
  }
}

export class AecStudioLocalApiHttpError extends Error {
  override readonly name = "AecStudioLocalApiHttpError";
  constructor(
    public readonly status: number,
    public readonly body: AecStudioLocalApiError,
  ) {
    super(`AEC Studio local API ${status}: ${body.code} (${body.error})`);
  }
}

/** Default per-request timeout. */
export const DEFAULT_TIMEOUT_MS = 5_000;

export interface AecStudioLocalApiClientOptions {
  /** Discovery record (port + token) — usually read from the port file. */
  portFile: AecStudioPortFileV1;
  /** Override fetch (tests). Default uses `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
  /** Override per-request timeout. */
  timeoutMs?: number;
}

export class AecStudioLocalApiClient {
  private readonly baseUrl: string;
  private readonly token: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;

  constructor(opts: AecStudioLocalApiClientOptions) {
    if (opts.portFile.host !== "127.0.0.1") {
      throw new AecStudioLocalApiUnavailableError(
        "AEC Studio local API host is not 127.0.0.1; refusing to connect.",
      );
    }
    if (!Number.isInteger(opts.portFile.port) || opts.portFile.port <= 0) {
      throw new AecStudioLocalApiUnavailableError(
        "AEC Studio local API port is invalid.",
      );
    }
    if (
      !opts.portFile.token ||
      opts.portFile.token.length < MIN_TOKEN_LENGTH
    ) {
      // Defence in depth: the port-file reader (`readPortFile()`)
      // applies the same check before constructing this client, but
      // a test seam that wires `props.client` directly into the
      // publish-panel view bypasses that path. Enforcing the same
      // floor here keeps the contract uniform and prevents a future
      // contributor from accidentally exercising the client with a
      // shorter-than-production token.
      throw new AecStudioLocalApiUnavailableError(
        `AEC Studio local API token is missing or shorter than ${MIN_TOKEN_LENGTH} characters.`,
      );
    }
    this.baseUrl = `http://127.0.0.1:${opts.portFile.port}`;
    this.token = opts.portFile.token;
    this.fetchImpl = opts.fetchImpl ?? globalThis.fetch.bind(globalThis);
    this.timeoutMs = opts.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  }

  status(): Promise<AecStudioLocalApiStatus> {
    return this.request<AecStudioLocalApiStatus>("GET", "/api/status");
  }

  async fetchQueuedPublishes(): Promise<readonly QueuedPublish[]> {
    const response = await this.request<QueuedPublishesResponse>(
      "GET",
      "/api/queued-publishes",
    );
    return response.queued;
  }

  acknowledgePublish(
    req: PublishToThreadRequest,
  ): Promise<PublishToThreadResponse> {
    return this.request<PublishToThreadResponse>(
      "POST",
      "/api/publish-to-thread",
      req,
    );
  }

  pushReviewComments(
    req: ReviewCommentsRequest,
  ): Promise<ReviewCommentsResponse> {
    return this.request<ReviewCommentsResponse>(
      "POST",
      "/api/review-comments",
      req,
    );
  }

  reviewsSnapshot(): Promise<ReviewsSnapshotResponse> {
    return this.request<ReviewsSnapshotResponse>("GET", "/api/reviews");
  }

  private async request<T>(
    method: "GET" | "POST",
    path: string,
    body?: unknown,
  ): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
        method,
        headers: {
          authorization: `Bearer ${this.token}`,
          accept: "application/json",
          ...(body !== undefined
            ? { "content-type": "application/json" }
            : {}),
        },
        body: body !== undefined ? JSON.stringify(body) : undefined,
        redirect: "error",
        signal: controller.signal,
      });
      if (!response.ok) {
        const errorBody = await safeJson<AecStudioLocalApiError>(response, {
          error: response.statusText,
          code: "internal_error",
        });
        throw new AecStudioLocalApiHttpError(response.status, errorBody);
      }
      return (await response.json()) as T;
    } catch (err) {
      if (
        err instanceof AecStudioLocalApiHttpError ||
        err instanceof AecStudioLocalApiUnavailableError
      ) {
        throw err;
      }
      const message =
        err instanceof Error ? err.message : String(err);
      throw new AecStudioLocalApiUnavailableError(
        `AEC Studio local API request failed: ${message}`,
      );
    } finally {
      clearTimeout(timer);
    }
  }
}

async function safeJson<T>(response: Response, fallback: T): Promise<T> {
  try {
    return (await response.json()) as T;
  } catch {
    return fallback;
  }
}
