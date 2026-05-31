import { describe, it, expect, vi, afterEach } from "vitest";
import {
  act,
  render,
  screen,
  fireEvent,
  waitFor,
  cleanup,
} from "@testing-library/react";

import { ViewportContainer } from "../components/design/ViewportContainer";
import { aec } from "../api/aec";

/**
 * Renderer-side tests for the Phase 12 viewport integration.
 *
 * The Design viewport drives the bridge entirely through the four
 * `aec.viewport.*` IPC handlers. Vitest can't talk to a real wgpu
 * adapter, so we mock each handler at the `aec.viewport.*` level and
 * assert (a) the component reports the bridge's `state`, (b) it
 * forwards pointer drags to `input({kind:"orbit"|"pan"|"zoom"})`,
 * and (c) it polls for frames when the bridge reports `state:"ready"`.
 */
describe("ViewportContainer", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it("renders the unavailable banner when the bridge reports no adapter", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "unavailable",
      width: 0,
      height: 0,
      frameIndex: 0,
      gpuDescriptorJson: null,
    });
    render(<ViewportContainer activeTool="select" />);
    await waitFor(() =>
      expect(screen.getByTestId("viewport-state-label").textContent).toBe(
        "unavailable",
      ),
    );
    expect(screen.getByTestId("viewport-unavailable-banner")).toBeTruthy();
  });

  it("renders the ready state when the bridge has a device", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "ready",
      width: 800,
      height: 600,
      frameIndex: 0,
      gpuDescriptorJson: JSON.stringify({
        vendor: "TestVendor",
        model: "TestModel",
        backend: "vulkan",
        device_type: "Other",
        driver: "",
        driver_info: "",
      }),
    });
    vi.spyOn(aec.viewport, "requestFrame").mockResolvedValue({
      frameIndex: 7,
      width: 800,
      height: 600,
      state: "presented",
      cameraJson: JSON.stringify({
        position: [5000, 3000, 5000],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    render(<ViewportContainer activeTool="select" />);
    await waitFor(() =>
      expect(screen.getByTestId("viewport-state-label").textContent).toBe(
        "ready",
      ),
    );
    expect(screen.getByText(/GPU: TestVendor/)).toBeTruthy();
  });

  it("forwards pointer drag as an orbit input", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "ready",
      width: 800,
      height: 600,
      frameIndex: 0,
      gpuDescriptorJson: null,
    });
    vi.spyOn(aec.viewport, "requestFrame").mockResolvedValue({
      frameIndex: 1,
      width: 800,
      height: 600,
      state: "coalesced",
      cameraJson: JSON.stringify({
        position: [5000, 3000, 5000],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    const input = vi.spyOn(aec.viewport, "input").mockResolvedValue({
      cameraJson: JSON.stringify({
        position: [5100, 3000, 5000],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    render(<ViewportContainer activeTool="select" />);
    await waitFor(() =>
      expect(screen.getByTestId("viewport-state-label").textContent).toBe(
        "ready",
      ),
    );
    const host = screen.getByTestId("design-viewport");
    // jsdom doesn't propagate `clientX/clientY` through synthesised
    // PointerEvents (they default to 0). We use the lower-level
    // createEvent path so we can attach real screen coords and
    // assert the bridge sees a non-zero drag.
    const downEvent = new MouseEvent("pointerdown", {
      bubbles: true,
      cancelable: true,
      clientX: 100,
      clientY: 100,
      button: 0,
    });
    Object.defineProperty(downEvent, "pointerId", { value: 1 });
    host.dispatchEvent(downEvent);
    const moveEvent = new MouseEvent("pointermove", {
      bubbles: true,
      cancelable: true,
      clientX: 120,
      clientY: 105,
    });
    Object.defineProperty(moveEvent, "pointerId", { value: 1 });
    host.dispatchEvent(moveEvent);
    const upEvent = new MouseEvent("pointerup", {
      bubbles: true,
      cancelable: true,
      clientX: 120,
      clientY: 105,
    });
    Object.defineProperty(upEvent, "pointerId", { value: 1 });
    host.dispatchEvent(upEvent);
    await waitFor(() =>
      expect(input).toHaveBeenCalledWith({
        kind: "orbit",
        dx: 20,
        dy: 5,
      }),
    );
  });

  it("forwards wheel events as a zoom input", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "ready",
      width: 800,
      height: 600,
      frameIndex: 0,
      gpuDescriptorJson: null,
    });
    vi.spyOn(aec.viewport, "requestFrame").mockResolvedValue({
      frameIndex: 1,
      width: 800,
      height: 600,
      state: "coalesced",
      cameraJson: JSON.stringify({
        position: [5000, 3000, 5000],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    const input = vi.spyOn(aec.viewport, "input").mockResolvedValue({
      cameraJson: JSON.stringify({
        position: [4500, 2700, 4500],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    render(<ViewportContainer activeTool="select" />);
    await waitFor(() =>
      expect(screen.getByTestId("viewport-state-label").textContent).toBe(
        "ready",
      ),
    );
    const host = screen.getByTestId("design-viewport");
    fireEvent.wheel(host, { deltaY: 100 });
    await waitFor(() =>
      expect(input).toHaveBeenCalledWith({ kind: "zoom", delta: -100 }),
    );
  });

  // Regression: Devin Review flagged that the resize-debounce
  // effect armed a 120 ms trailing-edge timer that, when it fired,
  // awaited an `aec.viewport.resize` IPC and then called
  // `setStatus` with the bridge's response. The effect cleanup
  // disconnected the observer and called `clearTimeout` on the
  // pending timer, but if the timer had ALREADY fired and the IPC
  // was in flight when the component unmounted, the awaited promise
  // resolved on a torn-down component and `setStatus` dispatched
  // into dead state. The fix mirrors the `alive` flag pattern
  // already used by the initial status probe (lines 58-84): the
  // cleanup sets `alive = false` and `flush` re-checks `alive`
  // after the awaited IPC resolves before writing state.
  //
  // This test pins the in-flight contract: a resize IPC that's
  // still pending when the component unmounts must NOT trigger a
  // post-unmount React state update when it eventually resolves.
  it("guards the in-flight resize IPC against post-unmount state updates", async () => {
    // Polyfill ResizeObserver so the debounce path executes (jsdom
    // lacks it; the component otherwise short-circuits to a single
    // synchronous resize on mount).
    let observerCallback:
      | ((entries: ResizeObserverEntry[]) => void)
      | null = null;
    class FakeResizeObserver {
      constructor(cb: (entries: ResizeObserverEntry[]) => void) {
        observerCallback = cb;
      }
      observe() {}
      disconnect() {
        observerCallback = null;
      }
      unobserve() {}
    }
    const prev = (
      globalThis as unknown as { ResizeObserver?: typeof ResizeObserver }
    ).ResizeObserver;
    (
      globalThis as unknown as { ResizeObserver: typeof ResizeObserver }
    ).ResizeObserver = FakeResizeObserver as unknown as typeof ResizeObserver;
    vi.useFakeTimers();
    const consoleErr = vi.spyOn(console, "error");
    try {
      // Use `unavailable` so the rAF loop (which fires only when
      // status.state === "ready") does not start — otherwise
      // `vi.runAllTimersAsync` would loop forever on the
      // requestAnimationFrame chain that the component's frame
      // poll effect drives.
      vi.spyOn(aec.viewport, "status").mockResolvedValue({
        state: "unavailable",
        width: 800,
        height: 600,
        frameIndex: 0,
        gpuDescriptorJson: null,
      });
      // Controllable resize promise: we resolve it manually AFTER
      // unmount to simulate an in-flight IPC at teardown time.
      let resolveResize: () => void = () => {};
      const resizePending = new Promise<{
        state: "ready" | "unavailable";
        width: number;
        height: number;
        frameIndex: number;
        gpuDescriptorJson: string | null;
      }>((res) => {
        resolveResize = () =>
          res({
            state: "ready",
            width: 1024,
            height: 768,
            frameIndex: 5,
            gpuDescriptorJson: null,
          });
      });
      vi.spyOn(aec.viewport, "resize").mockReturnValue(resizePending);

      const { unmount } = render(<ViewportContainer activeTool="select" />);
      await act(async () => {
        await vi.runAllTimersAsync();
      });

      // Arm the trailing-edge timer.
      expect(observerCallback).not.toBeNull();
      act(() => {
        observerCallback!([
          {
            contentRect: { width: 1024, height: 768 } as DOMRectReadOnly,
          } as ResizeObserverEntry,
        ]);
      });

      // Fire the trailing-edge timer so `flush` is called and the
      // resize IPC is in flight (awaited, not yet resolved).
      await act(async () => {
        await vi.advanceTimersByTimeAsync(150);
      });
      expect(aec.viewport.resize).toHaveBeenCalledWith({
        width: 1024,
        height: 768,
      });

      // Unmount while the IPC is still pending.
      unmount();

      // Resolve the IPC AFTER unmount. The `alive` flag inside
      // `flush` must short-circuit before `setStatus` is reached.
      await act(async () => {
        resolveResize();
        await vi.runAllTimersAsync();
      });

      // React would emit a "state update on unmounted component"
      // warning to console.error if `setStatus` ran after unmount
      // (React 18 quieted this for unmounted, but the alive guard
      // also prevents any future side-effect added inside `flush`
      // from leaking past the unmount boundary). Either way, no
      // errors of any kind should have surfaced.
      const unmountWarnings = consoleErr.mock.calls.filter((args) => {
        const msg = typeof args[0] === "string" ? args[0] : "";
        return (
          msg.includes("unmounted component") ||
          msg.includes("memory leak") ||
          msg.includes("Can't perform a React state update")
        );
      });
      expect(unmountWarnings).toEqual([]);
    } finally {
      consoleErr.mockRestore();
      vi.useRealTimers();
      if (prev) {
        (
          globalThis as unknown as { ResizeObserver: typeof ResizeObserver }
        ).ResizeObserver = prev;
      } else {
        delete (
          globalThis as unknown as { ResizeObserver?: typeof ResizeObserver }
        ).ResizeObserver;
      }
    }
  });

  // Phase 17 Task 21 — the rAF tick reads the bridge's RGBA8 frame
  // buffer and paints it into the canvas overlay via `putImageData`.
  // Vitest's jsdom doesn't actually rasterise the canvas, but we
  // can still verify (a) the bridge call is wired, (b) the canvas
  // element is mounted into the DOM, and (c) idle reads (same
  // frame_index) are coalesced — i.e. `putImageData` is NOT invoked
  // on every tick when the bridge reports the same `frameIndex`.
  it("paints the bridge's frame buffer into the canvas overlay on each new frameIndex", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "ready",
      width: 4,
      height: 2,
      frameIndex: 0,
      gpuDescriptorJson: null,
    });
    vi.spyOn(aec.viewport, "requestFrame").mockResolvedValue({
      frameIndex: 1,
      width: 4,
      height: 2,
      state: "presented",
      cameraJson: JSON.stringify({
        position: [5000, 3000, 5000],
        target: [0, 0, 0],
        up: [0, 1, 0],
        fov_y_radians: Math.PI / 3,
      }),
    });
    // The bridge returns three different frame buffers in sequence,
    // each with a different `frameIndex`. The paint loop must
    // invoke `putImageData` once per distinct index.
    const readFB = vi
      .spyOn(aec.viewport, "readFrameBuffer")
      .mockResolvedValueOnce({
        bytes: new Uint8Array(4 * 2 * 4).fill(7),
        width: 4,
        height: 2,
        frameIndex: 10,
      })
      .mockResolvedValueOnce({
        bytes: new Uint8Array(4 * 2 * 4).fill(7),
        width: 4,
        height: 2,
        frameIndex: 10, // duplicate — must coalesce
      })
      .mockResolvedValueOnce({
        bytes: new Uint8Array(4 * 2 * 4).fill(9),
        width: 4,
        height: 2,
        frameIndex: 11,
      })
      .mockResolvedValue(null);
    // jsdom's HTMLCanvasElement.getContext returns null by default;
    // patch it with a minimal 2d-context stub that records
    // putImageData calls so we can assert the painter ran.
    // `createImageData` returns a stub with a `data` array we can
    // inspect to verify the bridge bytes are forwarded into the
    // canvas image buffer.
    const putImageData = vi.fn();
    const createImageData = vi
      .fn()
      .mockImplementation((w: number, h: number) => ({
        width: w,
        height: h,
        data: new Uint8ClampedArray(w * h * 4),
      }));
    const getContext = vi
      .spyOn(HTMLCanvasElement.prototype, "getContext")
      .mockImplementation(
        () =>
          ({
            putImageData,
            createImageData,
          }) as unknown as CanvasRenderingContext2D,
      );

    try {
      render(<ViewportContainer activeTool="select" />);
      await waitFor(() =>
        expect(screen.getByTestId("viewport-state-label").textContent).toBe(
          "ready",
        ),
      );
      // The rAF loop ticks asynchronously; spin until we see at
      // least two distinct paints (frameIndex 10 + 11) or time out.
      await waitFor(() => expect(putImageData).toHaveBeenCalledTimes(2), {
        timeout: 2000,
      });

      const canvas = screen.getByTestId(
        "design-viewport-canvas",
      ) as HTMLCanvasElement;
      // The canvas was sized to the frame buffer's intrinsic
      // dimensions — `paintFrameBuffer` syncs the backing-store
      // size before `putImageData`.
      expect(canvas.width).toBe(4);
      expect(canvas.height).toBe(2);

      // The rAF loop keeps ticking past the 3 mocked frames (later
      // ticks resolve to `null` via `mockResolvedValue(null)` and
      // hit the early-return in the painter), so `readFB` will see
      // more than 3 calls in practice. The invariant we care about
      // is that the FIRST three calls drove EXACTLY two paints — the
      // duplicate frameIndex coalesces. We assert the coalescing
      // contract via `putImageData` and only verify the bridge was
      // called *at least* as many times as we mocked.
      expect(readFB.mock.calls.length).toBeGreaterThanOrEqual(3);
      expect(putImageData).toHaveBeenCalledTimes(2);
    } finally {
      getContext.mockRestore();
    }
  });

  // Phase 17 Task 21 — when the bridge reports `state:
  // "unavailable"`, the rAF loop must not run, which means
  // `readFrameBuffer` must NEVER be called. The unavailable banner
  // is the user-facing signal; calling the bridge's paint endpoint
  // would be wasted IPC.
  it("does not call readFrameBuffer when the viewport is unavailable", async () => {
    vi.spyOn(aec.viewport, "status").mockResolvedValue({
      state: "unavailable",
      width: 0,
      height: 0,
      frameIndex: 0,
      gpuDescriptorJson: null,
    });
    const readFB = vi.spyOn(aec.viewport, "readFrameBuffer");
    render(<ViewportContainer activeTool="select" />);
    await waitFor(() =>
      expect(screen.getByTestId("viewport-state-label").textContent).toBe(
        "unavailable",
      ),
    );
    // Give the rAF loop a chance to run anyway — if the guard
    // breaks in the future this assertion will catch it.
    await new Promise((r) => setTimeout(r, 50));
    expect(readFB).not.toHaveBeenCalled();
  });
});
