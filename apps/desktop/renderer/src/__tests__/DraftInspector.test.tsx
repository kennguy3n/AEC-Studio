import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { DraftInspector } from "../components/draft/DraftInspector";

describe("DraftInspector", () => {
  it("reports no selection when count is 0", () => {
    render(
      <DraftInspector
        activeTool="select"
        selection={{ count: 0, primaryType: null, layer: null }}
      />,
    );
    expect(screen.getByTestId("inspector-count").textContent).toBe("None");
  });

  it("pluralises entity count above 1", () => {
    render(
      <DraftInspector
        activeTool="select"
        selection={{ count: 5, primaryType: "Line", layer: "Walls" }}
      />,
    );
    expect(screen.getByTestId("inspector-count").textContent).toBe("5 entities");
  });

  it("shows dim style controls when dim tool is active", () => {
    render(
      <DraftInspector
        activeTool="dim"
        selection={{ count: 0, primaryType: null, layer: null }}
      />,
    );
    expect(screen.getByTestId("inspector-dim-style")).toBeInTheDocument();
  });

  it("hides dim style controls otherwise", () => {
    render(
      <DraftInspector
        activeTool="select"
        selection={{ count: 0, primaryType: null, layer: null }}
      />,
    );
    expect(screen.queryByTestId("inspector-dim-style")).toBeNull();
  });
});
