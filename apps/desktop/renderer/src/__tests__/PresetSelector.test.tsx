import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { PresetSelector } from "../components/render/PresetSelector";

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
});
