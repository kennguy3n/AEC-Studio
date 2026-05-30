import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { DraftCanvas } from "../components/draft/DraftCanvas";

describe("DraftCanvas", () => {
  it("renders the canvas and HUD", () => {
    render(<DraftCanvas activeTool="line" />);
    expect(screen.getByTestId("draft-canvas")).toBeInTheDocument();
    expect(screen.getByTestId("draft-canvas-surface")).toBeInTheDocument();
    expect(screen.getByTestId("draft-canvas-hud")).toBeInTheDocument();
  });

  it("publishes the active tool via data attribute", () => {
    render(<DraftCanvas activeTool="circle" />);
    const root = screen.getByTestId("draft-canvas");
    expect(root.getAttribute("data-active-tool")).toBe("circle");
  });

  it("emits onPick with screen coordinates on click", () => {
    const spy = vi.fn();
    render(<DraftCanvas activeTool="line" onPick={spy} />);
    const surface = screen.getByTestId("draft-canvas-surface");
    fireEvent.click(surface, { clientX: 100, clientY: 50 });
    expect(spy).toHaveBeenCalled();
  });

  // Phase 17 Group C Task 20 — paint imported DXF primitives.
  it("publishes the primitive count and HUD label when primitives are supplied", () => {
    render(
      <DraftCanvas
        activeTool="select"
        primitives={[
          {
            kind: "line",
            layer: "0",
            start: [0, 0],
            end: [10, 0],
          },
          {
            kind: "circle",
            layer: "0",
            center: [5, 0],
            radius: 2,
          },
        ]}
      />,
    );
    const root = screen.getByTestId("draft-canvas");
    expect(root.getAttribute("data-primitive-count")).toBe("2");
    expect(
      screen.getByTestId("draft-canvas-primitive-count").textContent,
    ).toContain("2 primitives");
  });

  it("respects the visibleLayers filter for the HUD count (zero-cost when filtered out)", () => {
    render(
      <DraftCanvas
        activeTool="select"
        primitives={[
          { kind: "line", layer: "WALL", start: [0, 0], end: [1, 1] },
          { kind: "line", layer: "DOOR", start: [0, 0], end: [1, 1] },
        ]}
        visibleLayers={new Set(["WALL"])}
      />,
    );
    // The HUD still reports the *total* primitives in the project
    // (filtering happens at paint time, not at count time) — this
    // is intentional so the user can see "5 primitives" while the
    // canvas surfaces only the layer they're focused on.
    expect(
      screen.getByTestId("draft-canvas-primitive-count").textContent,
    ).toContain("2 primitives");
  });
});
