import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { aec } from "../api/aec";

/**
 * Create a project via the in-process backend so the
 * `RequireProject` route guard lets mode pages through.
 */
async function ensureProjectOpen() {
  await aec.project.createFromTemplate("apartment", "Test Project");
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <App />
    </MemoryRouter>,
  );
}

describe("App", () => {
  beforeEach(async () => {
    await ensureProjectOpen();
  });

  it("renders the mode rail with all seven modes", () => {
    renderAt("/");
    expect(screen.getAllByRole("link")).toHaveLength(7);
    expect(screen.getByText("Home")).toBeInTheDocument();
    expect(screen.getByText("Design")).toBeInTheDocument();
    expect(screen.getByText("Draft")).toBeInTheDocument();
    expect(screen.getByText("BIM")).toBeInTheDocument();
    expect(screen.getByText("Render")).toBeInTheDocument();
    expect(screen.getByText("Deliver")).toBeInTheDocument();
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("renders the settings page when navigating to /settings", () => {
    renderAt("/settings");
    expect(screen.getByTestId("settings-page")).toBeInTheDocument();
  });

  it("renders the design page when navigating to /design", async () => {
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });
    expect(screen.getByTestId("design-viewport")).toBeInTheDocument();
  });

  it("redirects unknown routes to Home", () => {
    renderAt("/no-such-route");
    expect(screen.getByText("AEC Studio")).toBeInTheDocument();
  });

  it("marks the active mode rail item", async () => {
    renderAt("/render");
    await waitFor(() => {
      const renderLink = screen
        .getAllByRole("link")
        .find((a) => a.getAttribute("href") === "/render");
      expect(renderLink?.className).toMatch(/is-active/);
    });
  });

  it("toggles design tool selection", async () => {
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("tool-wall")).toBeInTheDocument();
    });
    const wallBtn = screen.getByTestId("tool-wall");
    expect(wallBtn).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(wallBtn);
    expect(wallBtn).toHaveAttribute("aria-pressed", "true");
  });
});
