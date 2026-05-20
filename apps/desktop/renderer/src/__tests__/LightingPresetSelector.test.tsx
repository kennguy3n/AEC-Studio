import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  LIGHTING_PRESETS,
  LightingPresetSelector,
} from "../components/render/LightingPresetSelector";

describe("LightingPresetSelector", () => {
  it("renders every bundled preset", () => {
    render(
      <LightingPresetSelector active="daylight" onChange={() => undefined} />,
    );
    for (const p of LIGHTING_PRESETS) {
      expect(
        screen.getByTestId(`lighting-preset-tile-${p.id}`),
      ).toBeInTheDocument();
    }
  });

  it("marks the active tile via aria-pressed", () => {
    render(
      <LightingPresetSelector active="studio" onChange={() => undefined} />,
    );
    const active = screen.getByTestId("lighting-preset-tile-studio");
    expect(active.getAttribute("aria-pressed")).toBe("true");
    const inactive = screen.getByTestId("lighting-preset-tile-daylight");
    expect(inactive.getAttribute("aria-pressed")).toBe("false");
  });

  it("emits onChange when a different tile is clicked", () => {
    const onChange = vi.fn();
    render(
      <LightingPresetSelector active="daylight" onChange={onChange} />,
    );
    fireEvent.click(screen.getByTestId("lighting-preset-tile-golden_hour"));
    expect(onChange).toHaveBeenCalledWith("golden_hour");
  });

  it("shows the kelvin label on every tile", () => {
    render(
      <LightingPresetSelector active="daylight" onChange={() => undefined} />,
    );
    const overcast = screen.getByTestId("lighting-preset-tile-overcast");
    expect(overcast.textContent).toContain("6700 K");
  });

  it("ids match the Rust LightingPresetKind enum", () => {
    // The Rust side relies on these ids being stable. If you rename
    // one of them, update `crates/aec_render/src/lighting.rs::id()`
    // at the same time.
    const expected = [
      "warm_evening",
      "daylight",
      "studio",
      "golden_hour",
      "blue_twilight",
      "overcast",
    ];
    expect(LIGHTING_PRESETS.map((p) => p.id)).toEqual(expected);
  });
});
