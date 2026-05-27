import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
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

// Regression: Devin Review flagged that App.tsx's `close` callback
// awaited `closeProject()` with no try/catch — inconsistent with
// the sibling `save` / `undo` / `redo` handlers, all of which
// wrap their bridge calls in try/catch with a same-shaped error
// toast. Today the main process's `project:close` handler is
// synchronous (only `clearActiveProjectPath()`) so a rejection
// is unreachable, but the channel signature is async and any
// future enhancement (a `flushPendingChanges` await, a
// confirmation dialog, telemetry emission) creates a real
// failure surface. Without the catch the bridge error would
// surface as an unhandled promise rejection and `navigate("/")`
// would never run — leaving the user stuck. The fix wraps
// `closeProject()` in try/catch, surfaces the failure via the
// existing toast system, and STILL navigates afterwards so a
// partial close (renderer state may already be cleared) doesn't
// strand the user on a broken project view.
describe("App — close-project error handling", () => {
  let closeSpy = vi.spyOn(aec.project, "close");
  closeSpy.mockRestore();

  beforeEach(async () => {
    await ensureProjectOpen();
    closeSpy = vi.spyOn(aec.project, "close");
  });

  afterEach(() => {
    closeSpy.mockRestore();
  });

  it("surfaces an error toast when closeProject rejects and still navigates Home", async () => {
    closeSpy.mockRejectedValue(new Error("project DB busy"));
    renderAt("/design");
    // Wait for Design page to mount (RequireProject lets us
    // through once the active project resolves on mount).
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });
    // Fire the mod+w close-project shortcut. `useShortcut`
    // listens on the document; firing keydown there directly
    // mirrors the real browser dispatch (Electron menu accel
    // bypass is what `installApplicationMenu` in main.ts
    // guarantees for the production app).
    act(() => {
      fireEvent.keyDown(document, { key: "w", ctrlKey: true });
    });
    // The error toast must surface so the user sees the failure
    // (instead of a silent unhandled rejection + stuck UI).
    await waitFor(() => {
      expect(
        screen.getByText(/Close failed: project DB busy/i),
      ).toBeInTheDocument();
    });
    // Navigation still happens — partial close shouldn't strand
    // the user. The Design page must no longer be on screen.
    await waitFor(() => {
      expect(screen.queryByTestId("design-mode")).not.toBeInTheDocument();
    });
    expect(closeSpy).toHaveBeenCalledTimes(1);
  });

  it("does NOT toast on the happy path (close succeeds and navigates Home)", async () => {
    closeSpy.mockResolvedValue({ ok: true as const });
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });
    act(() => {
      fireEvent.keyDown(document, { key: "w", ctrlKey: true });
    });
    await waitFor(() => {
      expect(screen.queryByTestId("design-mode")).not.toBeInTheDocument();
    });
    // No "Close failed:" toast on success.
    expect(screen.queryByText(/Close failed/i)).not.toBeInTheDocument();
    expect(closeSpy).toHaveBeenCalledTimes(1);
  });
});
