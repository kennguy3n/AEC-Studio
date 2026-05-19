import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "../App";

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <App />
    </MemoryRouter>,
  );
}

describe("App", () => {
  it("renders the mode rail with all six modes", () => {
    renderAt("/");
    expect(screen.getAllByRole("link")).toHaveLength(6);
    expect(screen.getByText("Home")).toBeInTheDocument();
    expect(screen.getByText("Design")).toBeInTheDocument();
    expect(screen.getByText("Draft")).toBeInTheDocument();
    expect(screen.getByText("BIM")).toBeInTheDocument();
    expect(screen.getByText("Render")).toBeInTheDocument();
    expect(screen.getByText("Deliver")).toBeInTheDocument();
  });

  it("renders the design page when navigating to /design", () => {
    renderAt("/design");
    expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    expect(screen.getByTestId("design-viewport")).toBeInTheDocument();
  });

  it("redirects unknown routes to Home", () => {
    renderAt("/no-such-route");
    expect(screen.getByText("AEC Studio")).toBeInTheDocument();
  });

  it("marks the active mode rail item", () => {
    renderAt("/render");
    const renderLink = screen
      .getAllByRole("link")
      .find((a) => a.getAttribute("href") === "/render");
    expect(renderLink?.className).toMatch(/is-active/);
  });

  it("toggles design tool selection", () => {
    renderAt("/design");
    const wallBtn = screen.getByTestId("tool-wall");
    expect(wallBtn).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(wallBtn);
    expect(wallBtn).toHaveAttribute("aria-pressed", "true");
  });
});
