# AEC Studio KChat Extension

A KChat Desktop (`.kcz`) extension that bridges [AEC Studio](https://github.com/kennguy3n/AEC-Studio)
and KChat Desktop without putting AEC Studio on the public network.

## What it does

The extension runs inside the KChat Desktop sandbox and talks to AEC
Studio over a loopback HTTP API the AEC Studio main process binds on
`127.0.0.1:<random-port>`. On activation the extension:

1. Reads the discovery file `{userData}/aec-kchat-port.json` to learn
   AEC Studio's current port and bearer token.
2. Polls `GET /api/queued-publishes` for artifact cards AEC Studio
   wants to publish to a KChat thread (deliver-pack shares, render
   bundles, BIM exports).
3. Posts each queued card into the thread via
   `invokeProcedure("kchat.send_message")` and acknowledges the result
   back to AEC Studio via `POST /api/publish-to-thread`.
4. On user gesture, queries the current channel via
   `invokeProcedure("kchat.query_messages")` and pushes the messages
   back to AEC Studio as review comments via
   `POST /api/review-comments`.

The extension does **not** talk to any network host other than
`127.0.0.1`. AEC Studio's bearer token is rotated on every restart and
the discovery file is written with mode `0600` so only the running
user can read it.

## Architecture

```
AEC Studio (Electron)                                KChat Desktop
+--------------------------+                        +-------------------+
| kchatLocalApi.ts         |  HTTP (loopback)       | aec-studio-kchat  |
|   GET  /api/status       | <--------------------  |   client.ts       |
|   GET  /api/queued-...   |                        |   portFile.ts     |
|   POST /api/publish-...  |                        |   host.ts         |
|   POST /api/review-...   |  -------------------+  |   publish-panel   |
|   GET  /api/reviews      |                     |  +-------------------+
+--------------------------+                     |        |
        ^                                        |        | invokeProcedure
        | aecstudio:// deeplinks                 |        v
+--------------------------+                     |  +-------------------+
| kchatDeeplinkBridge.ts   |  shell.openExternal +->| KChat host        |
|   aecstudio://review/...  | <------------------+    |   kchat.send_msg  |
+--------------------------+                          |   kchat.query_msg |
                                                      +-------------------+
```

For the canonical wire schema see
`apps/desktop/electron/kchat/kchatLocalApi.ts` in AEC Studio and the
hand-rolled mirror in `src/types.ts` here. The two MUST stay in sync.

## Build

```bash
npm install
npm run build         # tsc + .kcz bundle into releases/
npm test              # zip-writer unit tests
npm run typecheck     # tsc --noEmit
```

The `.kcz` output is deterministic and reproducible byte-for-byte:
`scripts/build.mjs` invokes a hand-rolled zip writer
(`scripts/zipWriter.mjs`) that sorts entries alphabetically and uses
fixed timestamps + permissions. A sibling `.sha256` file records the
exact hash for AEC Studio's release-signing pipeline to verify.

## Files

- `manifest.json` — declares procedures, the rightbar view, and the
  AEC Studio host contract (`portFile`, `minAecStudioVersion`).
- `src/types.ts` — hand-rolled wire mirror of AEC Studio's
  `LocalApiStatus`, `QueuedPublish`, `PublishToThread*`,
  `ReviewComments*`, `ReviewsSnapshot*`, and `PortFileV1` types.
- `src/portFile.ts` — pure port-file parser with injectable reader.
- `src/client.ts` — bearer-authenticated HTTP client with `redirect:
  "error"` and a 5-second timeout.
- `src/host.ts` — typed wrapper around `globalThis.__kchatHost`
  exposing `queryMessages`, `sendMessage`, `queryConversations`, and
  `openDeeplink`.
- `src/index.tsx` — `activate(ctx)` / `deactivate()` entry.
- `src/views/publish-panel.tsx` — the rightbar React view.
- `scripts/build.mjs` — deterministic `.kcz` builder.
- `scripts/fsWalk.mjs` / `scripts/zipWriter.mjs` — generic helpers.

## License

MIT. See the AEC Studio LICENSE for terms.
