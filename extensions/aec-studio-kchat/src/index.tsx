/**
 * Entry point for the AEC Studio KChat extension.
 *
 * The KChat Desktop extension runtime expects every `.kcz` bundle
 * to export an `activate(context)` function that wires its
 * contributed views and procedures, and an idempotent `deactivate()`
 * that tears everything down. This module is intentionally light:
 * most of the work happens lazily inside the rightbar view, since
 * (a) the AEC Studio loopback API may not be reachable at
 * activation time and (b) the activation hook is supposed to
 * return fast so it does not block the host's startup path.
 */
import { AecStudioPublishPanel } from "./views/publish-panel";

export interface KchatExtensionViewContext {
  /** The viewId declared in `manifest.json#views[].viewId`. */
  viewId: string;
  /** React-compatible element factory the host uses to render the view. */
  render(component: () => JSX.Element): void;
  /** AEC-Studio-aware bridge — see `./views/publish-panel.tsx`. */
  bridge: {
    readPortFile(): Promise<string | null>;
    openExternal(url: string): Promise<void>;
    currentChannelId: string | null;
    currentChannelName: string | null;
    currentTeamId: string | null;
  };
}

export interface KchatExtensionActivationContext {
  /** Register a view for one of the slots declared in the manifest. */
  registerView(
    viewId: string,
    factory: (ctx: KchatExtensionViewContext) => void,
  ): void;
  /** Append-only logger surfaced to KChat Desktop's developer console. */
  log(level: "info" | "warn" | "error", message: string): void;
}

/**
 * Module-scoped registration handles. Kept as a `Set` so a
 * misbehaving host that calls `activate` twice in a row doesn't
 * leak the previous registrations — `deactivate` runs every
 * teardown once and clears the set.
 */
const registrations = new Set<() => void>();

export function activate(ctx: KchatExtensionActivationContext): void {
  registrations.clear();
  ctx.registerView("aecstudio.publish-panel", (view) => {
    if (view.viewId !== "aecstudio.publish-panel") {
      ctx.log(
        "warn",
        `Unexpected view id: ${view.viewId}; rendering AEC Studio Publish anyway.`,
      );
    }
    view.render(() => <AecStudioPublishPanel bridge={view.bridge} />);
  });
  ctx.log("info", "AEC Studio KChat extension activated.");
}

export function deactivate(): void {
  for (const teardown of registrations) {
    try {
      teardown();
    } catch {
      // Continue tearing down the rest; extension dispose must be
      // best-effort because the host is on its shutdown path.
    }
  }
  registrations.clear();
}
