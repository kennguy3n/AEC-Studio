import { describe, expect, it, vi } from "vitest";
import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import {
  PanelResizeHandle,
  usePersistentPanelSize,
} from "../components/PanelResizeHandle";

/**
 * Phase 17 Group B Task 14 — Panel resize handle.
 *
 *   1. Pointer down + move emits a delta in pixels along the
 *      orientation axis. The parent is responsible for translating
 *      the delta to a clamped size.
 *   2. Lifecycle callbacks (`onResizeStart`/`End`) fire exactly
 *      once per drag.
 *   3. The handle reports orientation via `aria-orientation` so a
 *      screen reader announces "vertical separator, resize panel".
 *   4. `usePersistentPanelSize` round-trips through localStorage,
 *      clamps to min/max on every set, and tolerates corrupt
 *      stored values without throwing.
 */
describe("PanelResizeHandle", () => {
  it("emits the horizontal delta on drag (vertical orientation)", () => {
    const onResize = vi.fn();
    const onStart = vi.fn();
    const onEnd = vi.fn();
    render(
      <PanelResizeHandle
        orientation="vertical"
        onResize={onResize}
        onResizeStart={onStart}
        onResizeEnd={onEnd}
      />,
    );
    const handle = screen.getByTestId("panel-resize-handle");
    expect(handle.getAttribute("aria-orientation")).toBe("vertical");
    expect(handle.getAttribute("data-dragging")).toBe("false");
    act(() => {
      fireEvent.pointerDown(handle, { pointerId: 7, clientX: 100, clientY: 50 });
    });
    expect(onStart).toHaveBeenCalledTimes(1);
    expect(handle.getAttribute("data-dragging")).toBe("true");
    act(() => {
      fireEvent.pointerMove(handle, { pointerId: 7, clientX: 130, clientY: 70 });
    });
    expect(onResize).toHaveBeenCalledWith(30);
    act(() => {
      fireEvent.pointerMove(handle, { pointerId: 7, clientX: 90, clientY: 80 });
    });
    expect(onResize).toHaveBeenLastCalledWith(-10);
    act(() => {
      fireEvent.pointerUp(handle, { pointerId: 7, clientX: 90, clientY: 80 });
    });
    expect(onEnd).toHaveBeenCalledTimes(1);
    expect(handle.getAttribute("data-dragging")).toBe("false");
  });

  it("emits the vertical delta on drag (horizontal orientation)", () => {
    const onResize = vi.fn();
    render(
      <PanelResizeHandle
        orientation="horizontal"
        onResize={onResize}
      />,
    );
    const handle = screen.getByTestId("panel-resize-handle");
    expect(handle.getAttribute("aria-orientation")).toBe("horizontal");
    act(() => {
      fireEvent.pointerDown(handle, { pointerId: 1, clientX: 0, clientY: 200 });
    });
    act(() => {
      fireEvent.pointerMove(handle, { pointerId: 1, clientX: 50, clientY: 240 });
    });
    expect(onResize).toHaveBeenCalledWith(40);
  });

  it("ignores pointer events from other pointers mid-drag", () => {
    const onResize = vi.fn();
    render(
      <PanelResizeHandle orientation="vertical" onResize={onResize} />,
    );
    const handle = screen.getByTestId("panel-resize-handle");
    act(() => {
      fireEvent.pointerDown(handle, { pointerId: 3, clientX: 0, clientY: 0 });
    });
    act(() => {
      // Different pointerId — should be ignored.
      fireEvent.pointerMove(handle, { pointerId: 99, clientX: 100, clientY: 0 });
    });
    expect(onResize).not.toHaveBeenCalled();
  });
});

describe("usePersistentPanelSize", () => {
  it("returns default when nothing stored", () => {
    localStorage.clear();
    const { result } = renderHook(() =>
      usePersistentPanelSize("panel.test.unset", 240),
    );
    expect(result.current[0]).toBe(240);
  });

  it("round-trips via localStorage and clamps to min/max", () => {
    localStorage.clear();
    const { result } = renderHook(() =>
      usePersistentPanelSize("panel.test.rt", 300, { min: 100, max: 500 }),
    );
    act(() => result.current[1](400));
    expect(result.current[0]).toBe(400);
    expect(localStorage.getItem("panel.test.rt")).toBe("400");
    // Above max → clamps.
    act(() => result.current[1](9000));
    expect(result.current[0]).toBe(500);
    expect(localStorage.getItem("panel.test.rt")).toBe("500");
    // Below min → clamps.
    act(() => result.current[1](-10));
    expect(result.current[0]).toBe(100);
    expect(localStorage.getItem("panel.test.rt")).toBe("100");
  });

  it("recovers stored value on remount", () => {
    localStorage.clear();
    localStorage.setItem("panel.test.remount", "180");
    const { result } = renderHook(() =>
      usePersistentPanelSize("panel.test.remount", 240),
    );
    expect(result.current[0]).toBe(180);
  });

  it("falls back to default on corrupt stored value", () => {
    localStorage.clear();
    localStorage.setItem("panel.test.corrupt", "not-a-number");
    const { result } = renderHook(() =>
      usePersistentPanelSize("panel.test.corrupt", 240),
    );
    expect(result.current[0]).toBe(240);
  });

  it("clamps stored value above max on read", () => {
    localStorage.clear();
    localStorage.setItem("panel.test.over", "9000");
    const { result } = renderHook(() =>
      usePersistentPanelSize("panel.test.over", 240, {
        min: 100,
        max: 600,
      }),
    );
    expect(result.current[0]).toBe(600);
  });
});
