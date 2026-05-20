import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SpatialTree, SpatialNode } from "../components/bim/SpatialTree";

const TREE: SpatialNode = {
  id: "p",
  kind: "IfcProject",
  name: "Proj",
  children: [
    {
      id: "s",
      kind: "IfcSite",
      name: "Site",
      children: [
        {
          id: "b",
          kind: "IfcBuilding",
          name: "Building",
          children: [
            {
              id: "st",
              kind: "IfcBuildingStorey",
              name: "L1",
              children: [],
            },
          ],
        },
      ],
    },
  ],
};

describe("SpatialTree", () => {
  it("renders empty state when no root", () => {
    render(
      <SpatialTree
        root={null}
        selectedId={null}
        onSelect={() => undefined}
      />,
    );
    expect(screen.getByTestId("spatial-tree").textContent).toContain(
      "No model loaded",
    );
  });

  it("renders all nodes in the tree and reports selection", () => {
    const onSelect = vi.fn();
    render(<SpatialTree root={TREE} selectedId={null} onSelect={onSelect} />);
    expect(screen.getByTestId("spatial-node-p")).toBeInTheDocument();
    expect(screen.getByTestId("spatial-node-s")).toBeInTheDocument();
    expect(screen.getByTestId("spatial-node-b")).toBeInTheDocument();
    expect(screen.getByTestId("spatial-node-st")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("spatial-node-b"));
    expect(onSelect).toHaveBeenCalledWith("b");
  });

  it("toggles a subtree without firing selection", () => {
    const onSelect = vi.fn();
    render(<SpatialTree root={TREE} selectedId={null} onSelect={onSelect} />);
    expect(screen.getByTestId("spatial-node-b")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("spatial-toggle-s"));
    // After collapse, "b" node should no longer be in the DOM.
    expect(screen.queryByTestId("spatial-node-b")).toBeNull();
    expect(onSelect).not.toHaveBeenCalled();
  });
});
