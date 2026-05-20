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

  it("syncs the input when the parent rerenders with a different entity's value", () => {
    const { rerender } = render(
      <PropertyEditor
        entityId="ent_A"
        classification="IfcWall"
        psets={{ Pset_WallCommon: { FireRating: "F60" } }}
        onChange={() => undefined}
      />,
    );
    const input = screen.getByTestId(
      "pset-Pset_WallCommon-FireRating",
    ) as HTMLInputElement;
    expect(input.value).toBe("F60");
    // Parent switches to a different entity that happens to share the
    // same pset/key. The PsetRow instance is reused (key=propertyKey)
    // but `value` prop changes — the displayed draft must follow.
    rerender(
      <PropertyEditor
        entityId="ent_B"
        classification="IfcWall"
        psets={{ Pset_WallCommon: { FireRating: "F90" } }}
        onChange={() => undefined}
      />,
    );
    expect(input.value).toBe("F90");
  });

  it("does not overwrite an in-progress edit when value changes while focused", () => {
    const { rerender } = render(
      <PropertyEditor
        entityId="ent_A"
        classification="IfcWall"
        psets={{ Pset_WallCommon: { FireRating: "F60" } }}
        onChange={() => undefined}
      />,
    );
    const input = screen.getByTestId(
      "pset-Pset_WallCommon-FireRating",
    ) as HTMLInputElement;
    // User starts typing.
    input.focus();
    fireEvent.change(input, { target: { value: "F90-draft" } });
    expect(input.value).toBe("F90-draft");
    // Parent pushes a competing external update; the focused draft must survive.
    rerender(
      <PropertyEditor
        entityId="ent_A"
        classification="IfcWall"
        psets={{ Pset_WallCommon: { FireRating: "F60" } }}
        onChange={() => undefined}
      />,
    );
    expect(input.value).toBe("F90-draft");
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
