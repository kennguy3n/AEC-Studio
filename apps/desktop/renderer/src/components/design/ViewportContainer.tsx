import { useCallback, useEffect, useRef, useState } from "react";

import { aec } from "../../api/aec";
import type { DesignTool } from "./DesignToolbar";

interface Props {
  activeTool: DesignTool;
}

/**
 * 3D viewport host for Design mode.
 *
 * Drives the bridge's viewport service through the four
 * `aec.viewport.*` IPC handlers (status / resize / input /
 * requestFrame). The component:
 *
 * 1. Calls `status()` on mount to learn whether a GPU adapter is
 *    available and whether the bridge produced a real device.
 * 2. Observes its own `ResizeObserver` to forward viewport
 *    dimensions to the bridge — the off-screen surface is
 *    re-allocated when the host element changes size.
 * 3. Routes pointer drags to `viewport.input({ kind: "orbit" | ... })`
 *    so the camera is mutated server-side. The camera JSON the
 *    bridge echoes back is parsed and displayed in the overlay.
 * 4. Requests frames via `requestAnimationFrame` in a loop;
 *    `state === "coalesced"` short-circuits because the bridge's
 *    surface manager hashed the camera + viewport size and decided
 *    nothing changed.
 *
 * When `state === "unavailable"` (no adapter — vitest, CI without
 * llvmpipe, or a misbuilt native bridge) the component renders the
 * "GPU unavailable" overlay instead of running the rAF loop.
 */
export function ViewportContainer({ activeTool }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [status, setStatus] = useState<
    | { state: "loading" }
    | {
        state: "ready" | "unavailable";
        width: number;
        height: number;
        frameIndex: number;
        gpuDescriptor: { vendor: string; model: string } | null;
      }
  >({ state: "loading" });
  const [camera, setCamera] = useState<{
    position: [number, number, number];
    target: [number, number, number];
  }>({ position: [5000, 3000, 5000], target: [0, 0, 0] });
  const dragRef = useRef<{
    kind: "orbit" | "pan" | null;
    x: number;
    y: number;
  }>({ kind: null, x: 0, y: 0 });

  // 1. Initial status probe.
  useEffect(() => {
    let cancelled = false;
    aec.viewport
      .status()
      .then((s) => {
        if (cancelled) return;
        setStatus({
          state: s.state,
          width: s.width,
          height: s.height,
          frameIndex: s.frameIndex,
          gpuDescriptor: parseGpu(s.gpuDescriptorJson),
        });
      })
      .catch(() => {
        if (cancelled) return;
        setStatus({
          state: "unavailable",
          width: 0,
          height: 0,
          frameIndex: 0,
          gpuDescriptor: null,
        });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 2. ResizeObserver → viewport.resize (debounced).
  //
  // While a window or split-pane is being dragged the
  // ResizeObserver can fire 30+ times per second. Without
  // debouncing each fire issued a `viewport.resize` IPC + a
  // round-trip back to the bridge, which on a tier-2 laptop is
  // enough to drop the frame rate during a resize. We coalesce
  // bursts of resize events with a 120 ms trailing edge so the
  // bridge only re-allocates the surface once the user lets go.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    // jsdom (vitest) does not implement ResizeObserver. Fall back to
    // a single resize on mount in that environment so component
    // tests can still observe a `resize` IPC call without bringing
    // in a polyfill.
    if (typeof ResizeObserver === "undefined") {
      const w = Math.round(el.clientWidth || 800);
      const h = Math.round(el.clientHeight || 600);
      void aec.viewport.resize({ width: w, height: h }).catch(() => {});
      return;
    }

    let pendingTimer: ReturnType<typeof setTimeout> | null = null;
    let pendingW = 0;
    let pendingH = 0;
    // `alive` mirrors the same closure-captured flag used by the
    // initial status probe (lines 58-84). The trailing-edge timer
    // can fire AFTER the effect's cleanup ran (the timer queue and
    // the effect cleanup are independent), and the awaited
    // `aec.viewport.resize` IPC may still be in flight when React
    // unmounts the component. Without the guard, the `setStatus`
    // call inside `flush` is a silent no-op on a torn-down
    // component but it still produces a console warning under the
    // legacy renderer profile and, more importantly, the bridge
    // would receive a `resize` request for a viewport whose host
    // surface has already been torn down — wasting GPU memory the
    // surface manager has to reclaim on the next mount. Setting
    // `alive = false` in the cleanup short-circuits both the
    // bridge dispatch and the state write atomically.
    let alive = true;

    const flush = async () => {
      pendingTimer = null;
      if (!alive) return;
      const w = pendingW;
      const h = pendingH;
      if (w <= 0 || h <= 0) return;
      try {
        const s = await aec.viewport.resize({ width: w, height: h });
        if (!alive) return;
        setStatus((prev) =>
          prev.state === "loading"
            ? prev
            : {
                ...prev,
                state: s.state,
                width: s.width,
                height: s.height,
                frameIndex: s.frameIndex,
              },
        );
      } catch {
        // Resize is allowed to fail when the host loses the device
        // mid-flight (e.g. external GPU unplugged). Status stays at
        // its prior value; the next `requestFrame` will surface the
        // "unavailable" state if the bridge has indeed dropped.
      }
    };

    const ro = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const w = Math.round(entry.contentRect.width);
      const h = Math.round(entry.contentRect.height);
      if (w <= 0 || h <= 0) return;
      // Coalesce: store the latest dimensions and (re)arm the
      // trailing-edge timer.
      pendingW = w;
      pendingH = h;
      if (pendingTimer !== null) {
        clearTimeout(pendingTimer);
      }
      pendingTimer = setTimeout(() => {
        void flush();
      }, 120);
      // Optimistically update the resolution indicator so the
      // overlay reflects the *target* size during the drag — the
      // bridge will reconcile when `flush` resolves.
      setStatus((prev) =>
        prev.state === "loading" ? prev : { ...prev, width: w, height: h },
      );
    });
    ro.observe(el);
    return () => {
      alive = false;
      ro.disconnect();
      if (pendingTimer !== null) {
        clearTimeout(pendingTimer);
      }
    };
  }, []);

  // 3. Pointer handlers.
  const onPointerDown = useCallback((e: React.PointerEvent) => {
    const t = e.target as HTMLElement & {
      setPointerCapture?: (id: number) => void;
    };
    // jsdom (vitest) doesn't implement pointer capture; guard the
    // call so component tests don't blow up before reaching the
    // input-forwarding code path.
    if (typeof t.setPointerCapture === "function") {
      t.setPointerCapture(e.pointerId);
    }
    // Middle-button (or shift+left) pans; everything else orbits.
    dragRef.current = {
      kind: e.button === 1 || e.shiftKey ? "pan" : "orbit",
      x: e.clientX,
      y: e.clientY,
    };
  }, []);
  const onPointerMove = useCallback(async (e: React.PointerEvent) => {
    const drag = dragRef.current;
    if (drag.kind === null) return;
    const dx = e.clientX - drag.x;
    const dy = e.clientY - drag.y;
    if (dx === 0 && dy === 0) return;
    dragRef.current = { kind: drag.kind, x: e.clientX, y: e.clientY };
    try {
      const r = await aec.viewport.input({ kind: drag.kind, dx, dy });
      const cam = parseCamera(r.cameraJson);
      if (cam) setCamera(cam);
    } catch {
      // swallow — see resize-observer rationale above.
    }
  }, []);
  const onPointerUp = useCallback((e: React.PointerEvent) => {
    dragRef.current = { kind: null, x: 0, y: 0 };
    const t = e.target as HTMLElement & {
      releasePointerCapture?: (id: number) => void;
    };
    if (typeof t.releasePointerCapture === "function") {
      t.releasePointerCapture(e.pointerId);
    }
  }, []);
  const onWheel = useCallback(async (e: React.WheelEvent) => {
    // Wheel delta is "lines" in Firefox and pixels in Chrome. The
    // bridge applies a 0.001 scale internally; a 50-pixel delta
    // produces a sensible single-frame zoom step on both browsers.
    try {
      const r = await aec.viewport.input({
        kind: "zoom",
        delta: -e.deltaY,
      });
      const cam = parseCamera(r.cameraJson);
      if (cam) setCamera(cam);
    } catch {
      // swallow
    }
  }, []);

  // 4. rAF frame request loop. Only runs when state == "ready".
  useEffect(() => {
    if (status.state !== "ready") return;
    let running = true;
    let raf = 0;
    const tick = async () => {
      if (!running) return;
      try {
        const f = await aec.viewport.requestFrame();
        // The await above can race with unmount: by the time the
        // promise resolves the cleanup may have flipped `running` to
        // false and cancelled the in-flight rAF id. Re-check before
        // both the `setStatus` and the next `requestAnimationFrame`
        // so we don't (a) update React state on an unmounted host or
        // (b) schedule one more tick that would only return harmlessly
        // anyway.
        if (!running) return;
        setStatus((prev) =>
          prev.state === "loading"
            ? prev
            : { ...prev, frameIndex: f.frameIndex },
        );
      } catch {
        // swallow
      }
      if (!running) return;
      raf = requestAnimationFrame(() => {
        void tick();
      });
    };
    raf = requestAnimationFrame(() => {
      void tick();
    });
    return () => {
      running = false;
      cancelAnimationFrame(raf);
    };
  }, [status.state]);

  const unavailable = status.state === "unavailable";
  const loading = status.state === "loading";
  const ready = status.state === "ready";

  return (
    <section
      className="design-viewport"
      data-testid="design-viewport"
      data-active-tool={activeTool}
      data-viewport-state={status.state}
      aria-label="3D viewport"
      ref={containerRef}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onWheel={onWheel}
      style={{ touchAction: "none", position: "relative" }}
    >
      <div style={{ position: "absolute", top: 8, left: 8, fontSize: 12 }}>
        <div style={{ fontWeight: 600, fontSize: 13 }}>3D Viewport</div>
        <div style={{ color: "var(--aec-color-text-muted)" }}>
          Tool: {activeTool}
        </div>
        <div style={{ color: "var(--aec-color-text-muted)" }}>
          State: <span data-testid="viewport-state-label">{status.state}</span>
        </div>
        {ready && (
          <>
            <div style={{ color: "var(--aec-color-text-muted)" }}>
              Frame: {status.frameIndex}
            </div>
            {status.gpuDescriptor && (
              <div style={{ color: "var(--aec-color-text-muted)" }}>
                GPU: {status.gpuDescriptor.vendor} /{" "}
                {status.gpuDescriptor.model}
              </div>
            )}
            <div style={{ color: "var(--aec-color-text-muted)" }}>
              Camera: [{camera.position.map((p) => p.toFixed(0)).join(", ")}]
            </div>
          </>
        )}
      </div>
      {ready && (
        <div
          data-testid="viewport-resolution"
          style={{
            position: "absolute",
            bottom: 8,
            right: 8,
            fontSize: 11,
            color: "var(--aec-color-text-muted)",
            background: "rgba(0,0,0,0.35)",
            padding: "2px 6px",
            borderRadius: 4,
            pointerEvents: "none",
          }}
        >
          {status.width} × {status.height}
        </div>
      )}
      {loading && (
        <div data-testid="viewport-loading-banner" style={loadingStyle}>
          Initializing viewport…
        </div>
      )}
      {unavailable && (
        <div data-testid="viewport-unavailable-banner" style={loadingStyle}>
          GPU adapter unavailable — viewport disabled. Use the path tracer for
          renders.
        </div>
      )}
    </section>
  );
}

const loadingStyle: React.CSSProperties = {
  position: "absolute",
  inset: 0,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  background: "rgba(0,0,0,0.05)",
  color: "var(--aec-color-text-muted)",
  fontSize: 13,
  pointerEvents: "none",
};

function parseCamera(json: string): {
  position: [number, number, number];
  target: [number, number, number];
} | null {
  try {
    const parsed = JSON.parse(json) as {
      position?: [number, number, number];
      target?: [number, number, number];
    };
    if (!parsed.position || !parsed.target) return null;
    return { position: parsed.position, target: parsed.target };
  } catch {
    return null;
  }
}

function parseGpu(
  json: string | null | undefined,
): { vendor: string; model: string } | null {
  if (!json) return null;
  try {
    const parsed = JSON.parse(json) as { vendor?: string; model?: string };
    if (!parsed.vendor || !parsed.model) return null;
    return { vendor: parsed.vendor, model: parsed.model };
  } catch {
    return null;
  }
}
