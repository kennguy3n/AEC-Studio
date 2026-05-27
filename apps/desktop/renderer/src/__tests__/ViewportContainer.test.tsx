import { describe, it, expect, vi, afterEach } from "vitest";
import {
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
});
