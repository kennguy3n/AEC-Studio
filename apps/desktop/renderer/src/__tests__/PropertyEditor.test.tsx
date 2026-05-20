import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  PropertyEditor,
  PsetData,
} from "../components/bim/PropertyEditor";

const PSETS: PsetData = {
  Pset_WallCommon: {
    LoadBearing: true,
    FireRating: "F60",
    Thickness: 0.2,
  },
};

describe("PropertyEditor", () => {
  it("shows the empty placeholder when nothing is selected", () => {
    render(
      <PropertyEditor
        entityId={null}
        classification={null}
        psets={{}}
        onChange={() => undefined}
      />,
    );
    expect(screen.getByTestId("property-editor").textContent).toContain(
      "Select an element",
    );
  });

  it("renders one row per property of one pset", () => {
    render(
      <PropertyEditor
        entityId="ent_001"
        classification="IfcWall"
        psets={PSETS}
        onChange={() => undefined}
      />,
    );
    expect(screen.getByTestId("pset-Pset_WallCommon")).toBeInTheDocument();
    expect(
      screen.getByTestId("pset-Pset_WallCommon-LoadBearing"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("pset-Pset_WallCommon-FireRating"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("pset-Pset_WallCommon-Thickness"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("property-editor-class").textContent).toBe(
      "IfcWall",
    );
  });

  it("commits a text edit on blur with the typed value", async () => {
    const onChange = vi.fn();
    render(
      <PropertyEditor
        entityId="ent_001"
        classification="IfcWall"
        psets={PSETS}
        onChange={onChange}
      />,
    );
    const input = screen.getByTestId(
      "pset-Pset_WallCommon-FireRating",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "F90" } });
    fireEvent.blur(input);
    // Wait a microtask so the async commit resolves.
    await Promise.resolve();
    await Promise.resolve();
    expect(onChange).toHaveBeenCalledWith({
      Pset_WallCommon: { ...PSETS.Pset_WallCommon, FireRating: "F90" },
    });
  });
});
