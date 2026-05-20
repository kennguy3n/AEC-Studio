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
});
