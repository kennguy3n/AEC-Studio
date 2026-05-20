import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  PresetSelector,
  recommendedPresetFor,
} from "../components/render/PresetSelector";

describe("PresetSelector", () => {
  it("renders all seven preset rows", () => {
    render(<PresetSelector active="standard" onChange={() => undefined} />);
    for (const id of [
      "eevee_preview",
      "quick",
      "standard",
      "high",
      "studio",
      "walkthrough",
      "panorama",
    ]) {
      expect(screen.getByTestId(`preset-row-${id}`)).toBeInTheDocument();
    }
  });

  it("marks the active preset as checked", () => {
    render(<PresetSelector active="high" onChange={() => undefined} />);
    expect(
      (screen.getByTestId("preset-input-high") as HTMLInputElement).checked,
    ).toBe(true);
    expect(
      (screen.getByTestId("preset-input-standard") as HTMLInputElement).checked,
    ).toBe(false);
  });

  it("emits onChange when a different preset is picked", () => {
    const onChange = vi.fn();
    render(<PresetSelector active="standard" onChange={onChange} />);
    fireEvent.click(screen.getByTestId("preset-input-studio"));
    expect(onChange).toHaveBeenCalledWith("studio");
  });

  it("shows the recommended badge for the tier's default preset", () => {
    render(
      <PresetSelector
        active="quick"
        onChange={() => undefined}
        tier="Medium"
      />,
    );
    // Medium → standard per ARCHITECTURE.md §10.2
    expect(
      screen.getByTestId("preset-recommended-standard"),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId("preset-recommended-quick"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByTestId("preset-recommended-studio"),
    ).not.toBeInTheDocument();
  });

  it("renders no recommended badge when no tier is supplied", () => {
    render(<PresetSelector active="standard" onChange={() => undefined} />);
    expect(
      screen.queryByRole("status", { name: /Recommended/ }),
    ).not.toBeInTheDocument();
  });

  it("shows samples and resolution in the description", () => {
    render(<PresetSelector active="studio" onChange={() => undefined} />);
    const studioRow = screen.getByTestId("preset-row-studio");
    expect(studioRow.textContent).toContain("1024 samples");
    expect(studioRow.textContent).toContain("3840×2160");
  });

  it("recommendedPresetFor matches the Rust mapping", () => {
    expect(recommendedPresetFor("Low")).toBe("quick");
    expect(recommendedPresetFor("Medium")).toBe("standard");
    expect(recommendedPresetFor("High")).toBe("high");
    expect(recommendedPresetFor("Pro")).toBe("studio");
  });
});
