import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  CameraSelector,
  CameraTile,
} from "../components/render/CameraSelector";

const CAMS: CameraTile[] = [
  { id: "cam_a", name: "Cam A", preset: "wide_angle" },
  { id: "cam_b", name: "Cam B", preset: null },
];

describe("CameraSelector", () => {
  it("shows the empty state when no cameras saved", () => {
    render(
      <CameraSelector
        cameras={[]}
        selected={new Set()}
        onToggle={() => undefined}
      />,
    );
    expect(screen.getByTestId("camera-selector").textContent).toContain(
      "No saved cameras",
    );
  });

  it("renders one tile per saved camera and marks selected ones aria-pressed", () => {
    render(
      <CameraSelector
        cameras={CAMS}
        selected={new Set(["cam_b"])}
        onToggle={() => undefined}
      />,
    );
    expect(
      screen
        .getByTestId("camera-toggle-cam_a")
        .getAttribute("aria-pressed"),
    ).toBe("false");
    expect(
      screen
        .getByTestId("camera-toggle-cam_b")
        .getAttribute("aria-pressed"),
    ).toBe("true");
  });

  it("emits onToggle for the clicked camera", () => {
    const onToggle = vi.fn();
    render(
      <CameraSelector
        cameras={CAMS}
        selected={new Set()}
        onToggle={onToggle}
      />,
    );
    fireEvent.click(screen.getByTestId("camera-toggle-cam_a"));
    expect(onToggle).toHaveBeenCalledWith("cam_a");
  });
});
