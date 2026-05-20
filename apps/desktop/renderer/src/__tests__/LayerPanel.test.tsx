import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  LayerPanel,
  DEFAULT_LAYERS,
  LayerState,
} from "../components/draft/LayerPanel";

describe("LayerPanel", () => {
  it("renders one row per layer", () => {
    render(<LayerPanel layers={DEFAULT_LAYERS} onChange={() => {}} />);
    expect(screen.getByTestId("layer-row-0")).toBeInTheDocument();
    expect(screen.getByTestId("layer-row-Walls")).toBeInTheDocument();
    expect(screen.getByTestId("layer-row-Dims")).toBeInTheDocument();
  });

  it("toggles visibility through onChange", async () => {
    const spy = vi.fn<[next: LayerState[]], void>();
    render(<LayerPanel layers={DEFAULT_LAYERS} onChange={spy} />);
    const cb = screen.getByLabelText("Walls visible");
    fireEvent.click(cb);
    await waitFor(() => expect(spy).toHaveBeenCalled());
    const next = spy.mock.calls.at(-1)?.[0];
    const walls = next?.find((l: LayerState) => l.name === "Walls");
    expect(walls?.visible).toBe(false);
  });

  it("sets exactly one current layer", async () => {
    const spy = vi.fn<[next: LayerState[]], void>();
    render(<LayerPanel layers={DEFAULT_LAYERS} onChange={spy} />);
    fireEvent.click(screen.getByLabelText("Dims current"));
    await waitFor(() => expect(spy).toHaveBeenCalled());
    const next = spy.mock.calls.at(-1)?.[0];
    const currents = next?.filter((l: LayerState) => l.current);
    expect(currents?.length).toBe(1);
    expect(currents?.[0]?.name).toBe("Dims");
  });
});
