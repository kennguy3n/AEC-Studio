/**
 * Phase 15: shared shape of the JSON snapshot returned in
 * `KChatStatusReport.instanceJson` for the `loopback_http`
 * publisher.
 *
 * Mirrors `KchatRendererSnapshot` from
 * `apps/desktop/electron/kchat/kchatAppState.ts` exactly so a
 * field addition there surfaces here as a type error rather
 * than a silent `undefined` lookup on the consumer side.
 *
 * The interface and the parser used to live in two near-
 * identical copies (in `KChatStatusIndicator.tsx` and
 * `pages/Settings.tsx`); pulling them into a single module
 * means a new wire field only needs to be added in one place,
 * and the renderer never accidentally drops a field because
 * one copy fell behind the other.
 */
export interface KChatLoopbackInstance {
  apiServerRunning: boolean;
  apiServerPort: number | null;
  portFilePath: string | null;
  lastExtensionContactAt: string | null;
  queuedPublishCount: number;
  reviewThreadCount: number;
}

/**
 * Parse the `instanceJson` string from `kchat:status` /
 * `kchat:reload` into a typed `KChatLoopbackInstance`, or
 * return `null` when the field is missing, unparseable, or
 * structurally invalid.
 *
 * Accepts `string | null | undefined` to match both
 * `KChatStatusReport.instanceJson` (string | null) and the
 * test fixtures that sometimes pass `undefined` for "no
 * snapshot yet" rather than the wire `null`.
 *
 * The four numeric / boolean fields must be present and have
 * the right primitive type; the three nullable string fields
 * fall back to `null` when they are absent or a different
 * type. We do NOT throw on a partially-shaped object — the
 * snapshot wire format is allowed to grow new optional fields
 * without breaking older renderers.
 */
export function parseLoopbackInstance(
  json: string | null | undefined,
): KChatLoopbackInstance | null {
  if (!json) return null;
  try {
    const parsed = JSON.parse(json) as Partial<KChatLoopbackInstance>;
    if (
      typeof parsed.apiServerRunning !== "boolean" ||
      typeof parsed.queuedPublishCount !== "number" ||
      typeof parsed.reviewThreadCount !== "number"
    ) {
      return null;
    }
    return {
      apiServerRunning: parsed.apiServerRunning,
      apiServerPort:
        typeof parsed.apiServerPort === "number" ? parsed.apiServerPort : null,
      portFilePath:
        typeof parsed.portFilePath === "string" ? parsed.portFilePath : null,
      lastExtensionContactAt:
        typeof parsed.lastExtensionContactAt === "string"
          ? parsed.lastExtensionContactAt
          : null,
      queuedPublishCount: parsed.queuedPublishCount,
      reviewThreadCount: parsed.reviewThreadCount,
    };
  } catch {
    return null;
  }
}
