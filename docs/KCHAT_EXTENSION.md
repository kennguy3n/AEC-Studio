# AEC Studio KChat companion extension

The `.kcz` extension that lives inside KChat Desktop and bridges
the user's AEC Studio install with their KChat conversations.
Source lives at `extensions/aec-studio-kchat/`. The built artifact
is `releases/com.aecstudio.kchat-bridge@<version>.kcz` plus a
sidecar `.sha256` for the release-signing pipeline.

## Why an extension instead of direct IPC

KChat Desktop's [Extension Platform][platform-doc] is a JavaScript
sandbox running inside the renderer. There is **no socket
listener on the KChat side** — the platform was designed for
JS-only extensions that invoke a typed procedure registry, and
auditing `uneycom/uney-chat-desktop` (along with KCreate and
Tessera's pivot experience) confirmed the same architectural
shape. Phase 12 of AEC Studio shipped a UNIX-socket / named-pipe
transport against a hypothetical listener; Phase 15 deletes that
transport and replaces it with this extension.

The extension runs **inside** KChat Desktop's sandbox and talks
**outward** to AEC Studio's [loopback HTTP API][api-doc] over
`127.0.0.1`. AEC Studio is the server, the extension is the
client. Bidirectional navigation is handled with custom URL
schemes: `aecstudio://` (KChat → AEC Studio) and `kchat://` (AEC
Studio → KChat), both of which are registered with the host OS
via `setAsDefaultProtocolClient`.

[platform-doc]: https://github.com/uneycom/uney-chat-desktop/blob/main/docs/proposals/foundation/01-architecture.md
[api-doc]: KCHAT_LOOPBACK_API.md

## Capabilities the extension declares

`manifest.json#permissions.procedures` lists three calls the
extension is allowed to invoke against the KChat host:

| Procedure                  | Category | Why we need it |
|----------------------------|----------|----------------|
| `kchat.send_message`       | write    | Post an artifact card into the user-selected thread. |
| `kchat.query_messages`     | read     | Mirror recent thread messages back to AEC Studio as review comments. |
| `kchat.query_conversations`| read     | Surface a thread picker on the publish panel (planned; not yet wired). |

The extension declares one view in `manifest.json#views`:

* `aecstudio.publish-panel` in the `rightbar` slot.

## Host bridge contract

KChat Desktop injects `globalThis.__kchatHost` into the extension
sandbox. The bridge shape we rely on:

```ts
interface RawHostBridge {
  invokeProcedure(
    id: string,
    payload: unknown,
  ): Promise<{
    ok: boolean;
    value?: unknown;
    error?: { kind: string; message: string };
  }>;
  openDeeplink(url: string): Promise<{ ok: boolean; message?: string }>;
}
```

`src/host.ts` wraps this with:

* A guard that throws `HostProcedureError(EXTENSION_NOT_INSTALLED)`
  if the bridge is missing — useful when running unit tests with
  a stub.
* Typed procedure wrappers (`queryMessages`, `sendMessage`,
  `queryConversations`) so the React view never touches the raw
  bridge object.

`HostProcedureError.kind` is one of `EXTENSION_CAPABILITY_DENIED`,
`EXTENSION_NOT_INSTALLED`, `CONSENT_REQUIRED`, `RATE_LIMITED`,
`INVALID_REQUEST`, `HOST_INTERNAL_ERROR`,
`HOST_PROCEDURE_NOT_FOUND`. The view surfaces these verbatim so
the user knows whether the failure is a consent flow, a rate
limit, or a structural problem to escalate to AEC Studio support.

## Data flow

The view drives two flows:

### Publish

```
AEC Studio                      .kcz extension                   KChat Desktop
─────────────                    ──────────────                   ─────────────
queue card  ──────────────────▶
                  GET /api/queued-publishes (poll on activation +
                  on Refresh button)
                                send_message via __kchatHost  ─▶  post to thread
                                                                  ◀ messageId
                  POST /api/publish-to-thread  ◀──────────────
ack: drop from queue
```

Replay is safe: if the extension crashes between `kchat.send_message`
and `/api/publish-to-thread`, the next activation will replay the
card and KChat-side deduplication on `messageId` keeps the thread
clean.

### Review-comment mirror

```
KChat Desktop                   .kcz extension                  AEC Studio
─────────────                    ──────────────                  ──────────
user clicks "Mirror recent" on rightbar
                                query_messages(channelId, since)
                                  ──────────────────────────▶
                                  ◀ messages (host-redacted)
                  POST /api/review-comments  ──────────────────▶  append to thread
                                                                  audit row
```

The `since` filter is read from `GET /api/reviews`'s
`threads[].lastUpdatedAt`, so the second mirror only pulls the
delta. The host applies its redaction pipeline before returning
messages, so we never see private content the user didn't intend
to share with AEC Studio.

## Build

```bash
cd extensions/aec-studio-kchat
npm install
npm run typecheck     # tsc --noEmit, strict mode
npm run build         # tsc → dist/, then build a deterministic .kcz
npm test              # zip-writer unit tests
```

`scripts/build.mjs` is dependency-free: it reads `manifest.json`,
verifies every declared entry point exists in `dist/`, and emits
`releases/<id>@<version>.kcz` plus a `<id>@<version>.kcz.sha256`
sidecar. Two builds of the same source tree produce byte-equal
archives.

## Distribution

* The release-signing pipeline takes `releases/*.kcz` plus the
  `.sha256` sidecars and produces an Ed25519-signed manifest.
  Distribution lives in the AEC Studio installer.
* KChat Desktop only installs `.kcz` archives whose signature
  matches a trusted publisher key. Installing an unsigned
  `.kcz` requires the user to flip a developer-mode flag on the
  KChat side — useful for local QA but not the production path.

## Limits

* The extension never opens a socket. The only network calls it
  makes are to `http://127.0.0.1:<port>` with `redirect: "error"`
  and a 5-second timeout.
* The extension never touches the filesystem outside its sandbox.
  Reading the discovery file is delegated to KChat Desktop's host
  via `context.bridge.readPortFile()`, which constrains the path
  to the AEC-Studio-specific filename.
* The view only ever reads / writes data declared in the manifest.
  The host's consent gateway runs every `invokeProcedure` call
  through a per-procedure capability check.

## Versioning

* Extension version (`manifest.json#identity.version`) is
  semver-tagged independently from AEC Studio's app version.
* `manifest.json#host.aecStudioLocalApi.minAecStudioVersion`
  declares the floor for the loopback API surface the extension
  expects. Bumping the floor is a breaking change in the wire
  schema (e.g. removing a route or a required field); add new
  routes / optional fields without bumping the floor when
  possible.
