# AEC Studio loopback HTTP API

The AEC Studio Electron main process binds a small HTTP server on
`127.0.0.1:<kernel-assigned-port>` to talk to the AEC Studio
companion extension installed inside KChat Desktop. This document
is the canonical wire schema for that API. It is the contract the
`extensions/aec-studio-kchat` `.kcz` extension's `src/types.ts`
mirrors verbatim — drift here is a breaking change at the IPC
boundary.

## Lifecycle

1. AEC Studio starts (Electron main process).
2. `apps/desktop/electron/kchat/kchatLocalApi.ts` opens a
   `node:http` server with `host: "127.0.0.1"` and `port: 0`. The
   kernel picks the port. We never bind on `0.0.0.0`.
3. On `listening`, AEC Studio writes the **discovery file** —
   atomic `O_CREAT | O_EXCL | O_WRONLY` (or `rename`) — at:
   * macOS / Linux: `<userData>/aec-kchat-port.json`
   * Windows: `<userData>\aec-kchat-port.json`

   File mode is `0600` on POSIX. Contents:

   ```json
   {
     "version": 1,
     "host": "127.0.0.1",
     "port": 49321,
     "token": "<43-character base64url string, ≥ 32 bytes of entropy>",
     "startedAt": "2026-05-29T09:42:18.123Z",
     "pid": 18241
   }
   ```

4. The KChat Desktop `.kcz` extension reads this file on
   activation and on every reconnect attempt. It does **not**
   watch the file — the AEC Studio side rewrites it atomically on
   every restart, and the extension simply retries with the new
   contents on connection failure.

5. On AEC Studio shutdown, the loopback server is closed and the
   discovery file is removed (`unlink`). A best-effort cleanup
   only — a crashed AEC Studio process leaves a stale file; the
   extension treats `ECONNREFUSED` as the failure mode in that
   case and reports "AEC Studio is not running" without exposing
   the stale file.

## Authentication

Every request must carry `Authorization: Bearer <token>`. The
token is generated on every AEC Studio start with:

```ts
crypto.randomBytes(32).toString("base64url");
```

Comparison is **constant-time** via `crypto.timingSafeEqual`
against the bytes derived from the in-memory token. Missing or
malformed tokens return `401 unauthorized`.

## Host-header guard

`http.IncomingMessage.headers.host` MUST be one of:

* `127.0.0.1:<port>` (the canonical case)
* `localhost:<port>` (allowed for compatibility with WebKit
  developer-tool requests; resolved to loopback by every desktop
  OS)
* `[::1]:<port>` (IPv6 loopback)

Anything else returns `403 forbidden` with code `forbidden`. This
guards against DNS-rebinding attacks where a malicious page on
`https://attacker.example` sends `fetch("http://127.0.0.1:49321/",
{credentials: "omit"})` against the loopback API.

## Body cap

POST bodies are capped at **64 KiB**. Larger payloads return
`413 payload_too_large` before any JSON parse happens. The cap
applies after the request body has been fully buffered — we do
not stream-parse JSON.

## Routes

### `GET /api/status`

Snapshot of the AEC Studio side. The extension polls this on
activation and on user gesture; it does **not** poll continuously.

**Response (200, `application/json`)**

```json
{
  "aecStudioVersion": "0.15.0",
  "connected": true,
  "lastEventAt": "2026-05-29T09:41:50.000Z",
  "queuedPublishCount": 2,
  "capabilities": {
    "publishToThread": true,
    "ingestReviewComments": true,
    "deeplinks": true
  }
}
```

* `connected` — whether AEC Studio has an active project loaded
  and is willing to accept publishes. `false` means the extension
  should render the panel as idle but not show an error.
* `lastEventAt` — ISO-8601 timestamp of the most recent state
  change (queue mutation, project open, integration toggle). The
  extension uses this for a "Last sync: 3s ago" affordance.

### `GET /api/queued-publishes`

The queue of artifact cards AEC Studio wants to publish into a
KChat thread.

**Response (200, `application/json`)**

```json
{
  "queued": [
    {
      "cardId": "card-7d3f",
      "threadId": "kchat-thread-42",
      "body": "## Concept renders v3\n\n- Living room\n- Kitchen…",
      "cardJson": "{\"artifact\":\"ConceptRender\",…}",
      "queuedAt": "2026-05-29T09:30:11.000Z"
    }
  ]
}
```

The extension drains the queue in FIFO order. AEC Studio
de-duplicates by `cardId`, so a replay (e.g. the extension
posted to KChat but failed to call `/api/publish-to-thread`)
results in a duplicate `messageId` rather than a duplicate post.

### `POST /api/publish-to-thread`

Acknowledge that a queued card has been posted to a KChat
thread. The extension MUST call this within 5 s of receiving the
`messageId` from `kchat.send_message` — otherwise AEC Studio
will keep the card in the queue for the next activation cycle.

**Request body (`application/json`)**

```json
{
  "cardId": "card-7d3f",
  "threadId": "kchat-thread-42",
  "messageId": "kchat-msg-9991",
  "postedAt": "2026-05-29T09:30:13.555Z",
  "permalink": "kchat://app/conversation/kchat-thread-42/message/kchat-msg-9991"
}
```

* `permalink` is optional; AEC Studio renders it as a clickable
  link in the publish history panel when present.

**Response (200, `application/json`)**

```json
{ "ackId": "ack-9b2", "acknowledgedAt": "2026-05-29T09:30:13.700Z" }
```

If the `cardId` is unknown (e.g. AEC Studio already cleared the
queue) the route still returns `200` — the extension's job is
done. The `404 not_found` response is reserved for genuine route
mismatches.

### `POST /api/review-comments`

Push review comments AEC Studio fetched from KChat. The
extension calls this after invoking `kchat.query_messages` for a
thread.

**Request body (`application/json`)**

```json
{
  "threadId": "kchat-thread-42",
  "comments": [
    {
      "messageId": "kchat-msg-1001",
      "authorId": "kchat-user-7",
      "authorDisplayName": "alex@studio.example",
      "bodyMarkdown": "Can we move the couch?",
      "postedAt": "2026-05-29T09:42:01.000Z",
      "permalink": "kchat://app/conversation/kchat-thread-42/message/kchat-msg-1001"
    }
  ]
}
```

**Response (200, `application/json`)**

```json
{
  "threadId": "kchat-thread-42",
  "acceptedCount": 1,
  "acceptedAt": "2026-05-29T09:42:01.250Z"
}
```

AEC Studio de-duplicates by `messageId`, so a replay yields
`acceptedCount: 0`. The extension does not need to track which
comments it has already pushed.

### `GET /api/reviews`

A roll-up of every thread the extension has mirrored to AEC
Studio. The extension uses this on activation to know which
threads it has seen, so it can pass `since` to
`kchat.query_messages` and avoid re-pushing the entire history.

**Response (200, `application/json`)**

```json
{
  "threads": [
    {
      "threadId": "kchat-thread-42",
      "lastUpdatedAt": "2026-05-29T09:42:01.000Z",
      "commentCount": 7
    }
  ]
}
```

## Error envelope

Every non-2xx response uses the same JSON envelope:

```json
{
  "error": "human-readable message",
  "code": "unauthorized"
}
```

`code` is one of:

| code                  | HTTP | meaning |
|-----------------------|------|---------|
| `unauthorized`        | 401  | Missing / wrong bearer token. |
| `forbidden`           | 403  | Host header not loopback. |
| `invalid_request`     | 400  | Body failed schema validation. |
| `payload_too_large`   | 413  | Body exceeded 64 KiB. |
| `not_found`           | 404  | Unknown route. |
| `rate_limited`        | 429  | Reserved — not currently emitted. |
| `internal_error`      | 500  | Unexpected server-side fault. |
| `aec_unavailable`     | 503  | Reserved — emitted when no project is open. |

## Compatibility

* The discovery-file `version` field is `1` today. Bumping it
  requires a coordinated change in
  `extensions/aec-studio-kchat/src/portFile.ts` —
  `validatePortFile` rejects any version it doesn't recognise so
  an outdated extension fails closed.
* `manifest.json#host.aecStudioLocalApi.minAecStudioVersion`
  declares the floor the extension expects. AEC Studio refuses
  to serve queue snapshots to extensions whose declared `kcz`
  version pre-dates the current API surface (returns `503
  aec_unavailable`).

## Not exposed

Things AEC Studio deliberately does **not** put on the loopback
API:

* Project blobs (renders, IFC, DXF, PDFs). The extension can
  only see card metadata + a small body markdown excerpt.
* The audit chain. Review comments come *in* via
  `/api/review-comments`; the chain is private.
* Filesystem access of any kind. The extension cannot ask AEC
  Studio to read or write paths.
* Sidecar control (LLM, render). The extension cannot trigger
  inference or renders.

If a future phase needs any of those, add a new route here, mirror
the wire types in `extensions/aec-studio-kchat/src/types.ts`, and
update the canonical schema test fixtures in
`crates/aec_bridge/tests/` (see Phase 15 test suite for examples).
