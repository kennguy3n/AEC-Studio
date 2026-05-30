import { Viewport3DHost } from "../viewport/Viewport3DHost";
import type { DesignTool } from "./DesignToolbar";

interface Props {
  activeTool: DesignTool;
}

/**
 * Design mode's 3D viewport. Thin wrapper around the shared
 * [`Viewport3DHost`] (see `components/viewport/Viewport3DHost.tsx`)
 * — the wgpu surface wiring, ResizeObserver debounce, pointer/
 * wheel input forwarding, and rAF frame request loop all live in
 * the host. This wrapper exists to:
 *
 *   1. Keep the `DesignTool` type-narrowed at the call site
 *      (Bim mode's `activeTool` is the BIM tool union, not the
 *      Design tool union — making each wrapper carry its own typed
 *      prop avoids leaking either mode's tool type into the
 *      shared host).
 *   2. Preserve the existing `import { ViewportContainer } from
 *      "./components/design/ViewportContainer"` API used by
 *      `Design.tsx`, so the Group C refactor (extracting the shared
 *      host for BIM mode integration) didn't ripple through the
 *      Design page or its component tests.
 *
 * The shared host renders the outer element with
 * `data-testid="design-viewport"` and `className="design-viewport"`
 * when `mode === "design"` (the default), so the existing test +
 * CSS selectors continue to match unchanged.
 */
export function ViewportContainer({ activeTool }: Props) {
  return <Viewport3DHost mode="design" activeTool={activeTool} />;
}
