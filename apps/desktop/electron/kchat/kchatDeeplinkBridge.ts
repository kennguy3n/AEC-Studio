/**
 * `aecstudio://` deeplink protocol handler.
 *
 * Phase 15 registers AEC Studio as the default handler for the
 * `aecstudio://` URL scheme so KChat Desktop (and the `.kcz`
 * extension running inside it) can hand the user back to a
 * specific AEC Studio surface without round-tripping through
 * the OS shell. The routes currently understood are:
 *
 *   aecstudio://review?thread=<threadId>
 *     Open the Deliver review panel scoped to that thread.
 *
 *   aecstudio://project/<projectId>
 *     Open the project workspace by id (project root resolved
 *     from the registry — never a filesystem path so the URL
 *     can't escape the userData sandbox).
 *
 *   aecstudio://deliver/<packId>
 *     Open the Deliver page focused on the named pack.
 *
 * Platform behaviour:
 *
 *   - **macOS** delivers deeplinks via `app.on("open-url", …)`.
 *     Cocoa may fire the listener BEFORE `whenReady` resolves,
 *     in which case the URL is parked and replayed once a
 *     consumer registers.
 *   - **Windows / Linux** deliver deeplinks via a
 *     `second-instance` event on the primary process, with the
 *     raw URL as an entry in the spawned child's `argv`. The
 *     bridge claims the single-instance lock; the child
 *     forwards its argv and exits.
 *
 * Trust model: the URL parser is allow-list driven and rejects
 * any unrecognised host, query parameter, or pathological
 * character. SSRF / path-traversal / control-character tricks
 * (`aecstudio://../../etc/passwd`, query fragments with `<`/`>`,
 * 0x00-0x1F bytes, …) are scrubbed at parse time. The
 * downstream consumer receives only the typed `DeeplinkRoute`
 * union — the parser never hands raw strings to the filesystem.
 *
 * The implementation is intentionally test-friendly: the
 * protocol registration helpers accept injected Electron
 * primitives so tests can drive the dispatch logic without
 * spinning up a real Electron process.
 */

import type { App, Event as ElectronEvent } from "electron";

/** Custom URL scheme owned by AEC Studio. */
export const AEC_PROTOCOL_SCHEME = "aecstudio";

/** All routes the bridge recognises. */
export type DeeplinkRoute =
  | { kind: "review"; threadId: string }
  | { kind: "project"; projectId: string }
  | { kind: "deliver"; packId: string };

/** Why parsing failed. Tests assert on the discriminator. */
export type DeeplinkParseFailure =
  | "wrong-scheme"
  | "unknown-host"
  | "missing-id"
  | "invalid-characters"
  | "missing-query-param"
  | "trailing-segments"
  | "url-too-long";

export type DeeplinkParseResult =
  | { ok: true; route: DeeplinkRoute }
  | { ok: false; reason: DeeplinkParseFailure; detail?: string };

/** Hard cap; defends against pathological URLs from a malicious caller. */
const MAX_URL_LENGTH = 8 * 1024;

/** Allow [a-zA-Z0-9_-] only — KChat / project / pack ids fit this. */
const ID_PATTERN = /^[a-zA-Z0-9_-]{1,128}$/;

/**
 * Parse a single deeplink URL and return either a typed route or
 * a failure reason. Pure; safe to call from any context.
 */
export function parseDeeplink(rawUrl: string): DeeplinkParseResult {
  if (typeof rawUrl !== "string") {
    return { ok: false, reason: "wrong-scheme", detail: "not a string" };
  }
  if (rawUrl.length > MAX_URL_LENGTH) {
    return { ok: false, reason: "url-too-long" };
  }
  // Reject ASCII control characters (0x00-0x1F). They have no
  // legitimate place in a URL and rejecting them defeats
  // header- / protocol-injection attempts. eslint flags raw
  // control-char regexes; charCode scan instead.
  for (let i = 0; i < rawUrl.length; i++) {
    const code = rawUrl.charCodeAt(i);
    if (code <= 0x1f) {
      return { ok: false, reason: "invalid-characters" };
    }
  }
  let url: URL;
  try {
    url = new URL(rawUrl);
  } catch {
    return { ok: false, reason: "wrong-scheme", detail: "URL parser failed" };
  }
  if (url.protocol !== `${AEC_PROTOCOL_SCHEME}:`) {
    return {
      ok: false,
      reason: "wrong-scheme",
      detail: `expected ${AEC_PROTOCOL_SCHEME}:, got ${url.protocol}`,
    };
  }
  const host = url.hostname.toLowerCase();
  const segments = url.pathname
    .split("/")
    .filter((segment) => segment.length > 0);
  if (host === "review") {
    if (segments.length > 0) {
      return { ok: false, reason: "trailing-segments" };
    }
    const threadId = url.searchParams.get("thread");
    if (threadId === null || threadId.length === 0) {
      return {
        ok: false,
        reason: "missing-query-param",
        detail: "thread",
      };
    }
    if (!ID_PATTERN.test(threadId)) {
      return { ok: false, reason: "invalid-characters" };
    }
    return { ok: true, route: { kind: "review", threadId } };
  }
  if (host === "project") {
    if (segments.length === 0) {
      return { ok: false, reason: "missing-id", detail: "project id" };
    }
    if (segments.length > 1) {
      return { ok: false, reason: "trailing-segments" };
    }
    const projectId = decodeURIComponentSafe(segments[0]);
    if (projectId === null) {
      return { ok: false, reason: "invalid-characters" };
    }
    if (!ID_PATTERN.test(projectId)) {
      return { ok: false, reason: "invalid-characters" };
    }
    return { ok: true, route: { kind: "project", projectId } };
  }
  if (host === "deliver") {
    if (segments.length === 0) {
      return { ok: false, reason: "missing-id", detail: "deliver pack id" };
    }
    if (segments.length > 1) {
      return { ok: false, reason: "trailing-segments" };
    }
    const packId = decodeURIComponentSafe(segments[0]);
    if (packId === null) {
      return { ok: false, reason: "invalid-characters" };
    }
    if (!ID_PATTERN.test(packId)) {
      return { ok: false, reason: "invalid-characters" };
    }
    return { ok: true, route: { kind: "deliver", packId } };
  }
  return {
    ok: false,
    reason: "unknown-host",
    detail: host,
  };
}

function decodeURIComponentSafe(value: string): string | null {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
}

/**
 * Compose a deeplink URL for the given route. Inverse of
 * `parseDeeplink`; the test suite round-trips every variant.
 */
export function buildDeeplink(route: DeeplinkRoute): string {
  switch (route.kind) {
    case "review": {
      const url = new URL(`${AEC_PROTOCOL_SCHEME}://review`);
      url.searchParams.set("thread", route.threadId);
      return url.toString();
    }
    case "project":
      return `${AEC_PROTOCOL_SCHEME}://project/${encodeURIComponent(route.projectId)}`;
    case "deliver":
      return `${AEC_PROTOCOL_SCHEME}://deliver/${encodeURIComponent(route.packId)}`;
  }
}

export type DeeplinkConsumer = (route: DeeplinkRoute) => void;

/**
 * Stateful router used by the Electron main process. The bridge
 * keeps a queue of pre-ready deeplinks (Cocoa may dispatch an
 * `open-url` event before the renderer is up); calls to
 * `dispatch()` invoke the registered consumer if any, otherwise
 * park the route. Once a consumer registers, queued routes are
 * flushed in FIFO order.
 */
export class DeeplinkBridge {
  private consumer: DeeplinkConsumer | null = null;
  private readonly parked: DeeplinkRoute[] = [];
  private readonly parseFailureLogger: (
    raw: string,
    failure: DeeplinkParseFailure,
    detail?: string,
  ) => void;

  constructor(
    opts: {
      onParseFailure?: (
        raw: string,
        failure: DeeplinkParseFailure,
        detail?: string,
      ) => void;
    } = {},
  ) {
    this.parseFailureLogger = opts.onParseFailure ?? (() => undefined);
  }

  /** Register the consumer (usually the renderer-facing IPC pump). */
  setConsumer(consumer: DeeplinkConsumer): void {
    this.consumer = consumer;
    while (this.parked.length > 0) {
      const next = this.parked.shift();
      if (next === undefined) break;
      try {
        consumer(next);
      } catch {
        // Best-effort: swallow consumer errors so a buggy
        // renderer does not break the rest of the queue.
      }
    }
  }

  /** Detach the consumer (e.g. when the renderer window closes). */
  clearConsumer(): void {
    this.consumer = null;
  }

  /** Snapshot of currently-parked routes; tests assert on this. */
  parkedRoutes(): readonly DeeplinkRoute[] {
    return [...this.parked];
  }

  /** Number of parked dispatches; tests assert on this. */
  pendingCount(): number {
    return this.parked.length;
  }

  /**
   * Handle a raw URL. Invalid URLs are dropped after the failure
   * logger fires; valid URLs are forwarded to the consumer or
   * parked.
   */
  ingestRawUrl(rawUrl: string): DeeplinkParseResult {
    const result = parseDeeplink(rawUrl);
    if (!result.ok) {
      this.parseFailureLogger(rawUrl, result.reason, result.detail);
      return result;
    }
    this.dispatch(result.route);
    return result;
  }

  /** Forward a typed route to the consumer (or park if none). */
  dispatch(route: DeeplinkRoute): void {
    if (this.consumer !== null) {
      try {
        this.consumer(route);
      } catch {
        // Buggy consumer; the route is still considered
        // dispatched so we don't re-queue it.
      }
      return;
    }
    this.parked.push(route);
  }

  /** Extract the first `aecstudio://` URL from an argv vector. */
  static extractUrlFromArgv(argv: readonly string[]): string | null {
    for (const arg of argv) {
      if (
        typeof arg === "string" &&
        arg.toLowerCase().startsWith(`${AEC_PROTOCOL_SCHEME}://`)
      ) {
        return arg;
      }
    }
    return null;
  }
}

/**
 * Register the `aecstudio://` scheme with Electron so the OS
 * routes the scheme to this binary. Must be called BEFORE
 * `app.whenReady()`.
 *
 * Returns `true` when the registration took effect, or `false`
 * when another binary already owns it. In development we
 * additionally pass `process.execPath` + `process.argv[1]` so a
 * stand-alone `electron .` invocation owns the scheme instead
 * of the system Electron binary.
 */
export function registerProtocolClient(
  app: Pick<App, "setAsDefaultProtocolClient">,
  opts: {
    execPath?: string;
    args?: readonly string[];
  } = {},
): boolean {
  if (opts.execPath !== undefined) {
    return app.setAsDefaultProtocolClient(
      AEC_PROTOCOL_SCHEME,
      opts.execPath,
      [...(opts.args ?? [])],
    );
  }
  return app.setAsDefaultProtocolClient(AEC_PROTOCOL_SCHEME);
}

/**
 * Wire the bridge into Electron's main-process app events.
 * Returns a teardown function that removes the listeners — tests
 * use this to reset between cases without leaking handles.
 */
export function attachAppEvents(
  bridge: DeeplinkBridge,
  app: Pick<App, "on" | "off">,
): () => void {
  const openUrlListener = (event: ElectronEvent, url: string): void => {
    event.preventDefault?.();
    bridge.ingestRawUrl(url);
  };
  const secondInstanceListener = (
    _event: ElectronEvent,
    argv: readonly string[],
  ): void => {
    const url = DeeplinkBridge.extractUrlFromArgv(argv);
    if (url !== null) {
      bridge.ingestRawUrl(url);
    }
  };
  app.on("open-url", openUrlListener);
  app.on("second-instance", secondInstanceListener);
  return () => {
    app.off("open-url", openUrlListener);
    app.off("second-instance", secondInstanceListener);
  };
}
