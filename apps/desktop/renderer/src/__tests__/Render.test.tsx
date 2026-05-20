import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { Render } from "../pages/Render";

describe("Render page", () => {
  it("assembles preset, cameras, queue, doctor, preview, compare", () => {
    render(<Render />);
    expect(screen.getByTestId("render-mode")).toBeInTheDocument();
    expect(screen.getByTestId("preset-selector")).toBeInTheDocument();
    expect(screen.getByTestId("camera-selector")).toBeInTheDocument();
    expect(screen.getByTestId("render-queue")).toBeInTheDocument();
    expect(screen.getByTestId("render-doctor")).toBeInTheDocument();
    expect(screen.getByTestId("render-preview")).toBeInTheDocument();
    expect(screen.getByTestId("before-after-compare")).toBeInTheDocument();
  });
});
