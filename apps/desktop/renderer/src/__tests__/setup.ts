import "@testing-library/jest-dom/vitest";

/**
 * Phase 17 Group B Task 14 — jsdom does not ship a `PointerEvent`
 * constructor, so `@testing-library/react`'s `fireEvent.pointerDown`
 * / `pointerMove` / `pointerUp` end up dispatching a plain `Event`
 * that drops the init dictionary (`pointerId`, `clientX`, `clientY`,
 * …). Components that read pointer coordinates (e.g.
 * `PanelResizeHandle`) then observe `NaN` deltas in tests even
 * though they work in a real browser.
 *
 * Polyfill the class with a MouseEvent-backed shim so tests can
 * exercise the real pointer-event code paths. Only register the
 * shim when the host does not already provide one — Chromium-based
 * test runners do.
 */
if (typeof window !== "undefined" && typeof window.PointerEvent === "undefined") {
  class PointerEventShim extends MouseEvent {
    public readonly pointerId: number;
    public readonly pointerType: string;
    public readonly isPrimary: boolean;
    public readonly width: number;
    public readonly height: number;
    public readonly pressure: number;
    public readonly tangentialPressure: number;
    public readonly tiltX: number;
    public readonly tiltY: number;
    public readonly twist: number;

    constructor(type: string, params: PointerEventInit = {}) {
      super(type, params);
      this.pointerId = params.pointerId ?? 0;
      this.pointerType = params.pointerType ?? "mouse";
      this.isPrimary = params.isPrimary ?? true;
      this.width = params.width ?? 1;
      this.height = params.height ?? 1;
      this.pressure = params.pressure ?? 0;
      this.tangentialPressure = params.tangentialPressure ?? 0;
      this.tiltX = params.tiltX ?? 0;
      this.tiltY = params.tiltY ?? 0;
      this.twist = params.twist ?? 0;
    }
  }

  // Both `globalThis.PointerEvent` and `window.PointerEvent` need
  // to resolve to the shim — DOM APIs (`HTMLElement.setPointerCapture`)
  // and React's `SyntheticPointerEvent` use both lookup paths.
  (globalThis as unknown as { PointerEvent: typeof PointerEventShim }).PointerEvent =
    PointerEventShim;
  (window as unknown as { PointerEvent: typeof PointerEventShim }).PointerEvent =
    PointerEventShim;
}

/**
 * `HTMLElement.setPointerCapture` and `releasePointerCapture` are
 * also missing from jsdom. The real browser uses them to keep
 * pointer events flowing to a captured element even when the pointer
 * exits its bounds; in tests we don't need that, but the methods
 * must exist so calling code doesn't throw.
 */
if (typeof window !== "undefined" && window.HTMLElement) {
  const proto = window.HTMLElement.prototype as HTMLElement & {
    setPointerCapture?: (pointerId: number) => void;
    releasePointerCapture?: (pointerId: number) => void;
    hasPointerCapture?: (pointerId: number) => boolean;
  };
  if (typeof proto.setPointerCapture !== "function") {
    proto.setPointerCapture = function noopSetPointerCapture() {
      // jsdom stub; real browsers retarget events.
    };
  }
  if (typeof proto.releasePointerCapture !== "function") {
    proto.releasePointerCapture = function noopReleasePointerCapture() {
      // jsdom stub; real browsers stop retargeting events.
    };
  }
  if (typeof proto.hasPointerCapture !== "function") {
    proto.hasPointerCapture = function noopHasPointerCapture() {
      return false;
    };
  }
}
