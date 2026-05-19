import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MaterialPanel } from "../components/design/MaterialPanel";

describe("MaterialPanel", () => {
  it("renders all default materials", () => {
    render(<MaterialPanel />);
    expect(screen.getByTestId("material-wall_white")).toBeInTheDocument();
    expect(screen.getByTestId("material-wood_oak")).toBeInTheDocument();
    expect(screen.getByTestId("material-concrete_polished")).toBeInTheDocument();
  });

  it("opens inspector for selected material", () => {
    render(<MaterialPanel />);
    fireEvent.click(screen.getByTestId("material-wood_oak"));
    const inspector = screen.getByTestId("material-inspector");
    expect(inspector).toBeInTheDocument();
    expect(inspector.querySelector("h3")?.textContent).toBe("Oak");
  });
});
