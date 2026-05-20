import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { DraftToolbar } from "../components/draft/DraftToolbar";

describe("DraftToolbar", () => {
  it("renders draw and edit tool groups", () => {
    render(<DraftToolbar activeTool="select" onSelect={() => {}} />);
    expect(screen.getByTestId("draft-toolbar-draw")).toBeInTheDocument();
    expect(screen.getByTestId("draft-toolbar-edit")).toBeInTheDocument();
    expect(screen.getByTestId("draft-tool-line")).toBeInTheDocument();
    expect(screen.getByTestId("draft-tool-trim")).toBeInTheDocument();
  });

  it("highlights the active tool", () => {
    render(<DraftToolbar activeTool="line" onSelect={() => {}} />);
    const btn = screen.getByTestId("draft-tool-line");
    expect(btn.className).toContain("draft-toolbar__btn--active");
    const other = screen.getByTestId("draft-tool-circle");
    expect(other.className).not.toContain("draft-toolbar__btn--active");
  });

  it("emits onSelect when a tool button is clicked", () => {
    const spy = vi.fn();
    render(<DraftToolbar activeTool="select" onSelect={spy} />);
    fireEvent.click(screen.getByTestId("draft-tool-polyline"));
    expect(spy).toHaveBeenCalledWith("polyline");
  });

  it("emits select tool when Select clicked", () => {
    const spy = vi.fn();
    render(<DraftToolbar activeTool="line" onSelect={spy} />);
    fireEvent.click(screen.getByTestId("draft-tool-select"));
    expect(spy).toHaveBeenCalledWith("select");
  });
});
