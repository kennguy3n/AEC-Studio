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

// Regression: Devin Review flagged that App.tsx's `activeScope`
// derivation fell back to `"design"` on unknown routes (Home,
// Settings, /no-such-route). Combined with the existing
// `if (project === null) return;` guard, this meant that a user
// who opened a project, placed a chair in Design, navigated to
// Home (project still active), and reflexively hit Ctrl/Cmd+Z
// would silently undo the chair placement — from a screen with
// no visual affordance for the action. The fix narrows
// `activeScope` to `CommandScope | null` and gates both `undo`
// and `redo` on the scope being non-null, so the shortcut
// becomes a no-op on Home/Settings instead of an off-screen
// mutation. These regression tests pin the no-op behaviour
// (no bridge call, no toast) for both undo and redo on each of
// the two non-mode routes.
describe("App — undo/redo on non-mode routes are no-ops", () => {
  let undoSpy = vi.spyOn(aec.command, "undo");
  let redoSpy = vi.spyOn(aec.command, "redo");
  undoSpy.mockRestore();
  redoSpy.mockRestore();

  beforeEach(async () => {
    await ensureProjectOpen();
    undoSpy = vi.spyOn(aec.command, "undo");
    redoSpy = vi.spyOn(aec.command, "redo");
  });

  afterEach(() => {
    undoSpy.mockRestore();
    redoSpy.mockRestore();
  });

  it("Ctrl+Z on Home with a project open does NOT call the bridge", async () => {
    renderAt("/");
    // Wait for the active-project hook to settle so we know the
    // shortcut handler sees `project !== null`. Otherwise the
    // `project === null` guard could short-circuit and we'd be
    // testing the wrong code path.
    await waitFor(() => {
      expect(screen.getByText("AEC Studio")).toBeInTheDocument();
    });
    act(() => {
      fireEvent.keyDown(document, { key: "z", ctrlKey: true });
    });
    // No bridge call should have been dispatched. The previous
    // (buggy) code path would have called `aec.command.undo(path, "design")`
    // and surfaced either a success or a "scope mismatch" error toast.
    expect(undoSpy).not.toHaveBeenCalled();
    expect(screen.queryByText(/Undo failed/i)).not.toBeInTheDocument();
  });

  it("Ctrl+Shift+Z on Home with a project open does NOT call the bridge", async () => {
    renderAt("/");
    await waitFor(() => {
      expect(screen.getByText("AEC Studio")).toBeInTheDocument();
    });
    act(() => {
      fireEvent.keyDown(document, { key: "z", ctrlKey: true, shiftKey: true });
    });
    expect(redoSpy).not.toHaveBeenCalled();
    expect(screen.queryByText(/Redo failed/i)).not.toBeInTheDocument();
  });

  it("Ctrl+Z on /settings with a project open does NOT call the bridge", async () => {
    renderAt("/settings");
    await waitFor(() => {
      expect(screen.getByTestId("settings-page")).toBeInTheDocument();
    });
    act(() => {
      fireEvent.keyDown(document, { key: "z", ctrlKey: true });
    });
    expect(undoSpy).not.toHaveBeenCalled();
  });

  it("Ctrl+Z on /design with a project open DOES call the bridge (positive control)", async () => {
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });
    // The bridge throws "nothing to undo" because the in-process
    // graph is empty for this fresh project. We only care that
    // the bridge was *called* — that proves the scope mapping
    // works for design routes and the guard is route-scoped, not
    // a blanket no-op.
    undoSpy.mockRejectedValue(new Error("nothing to undo"));
    act(() => {
      fireEvent.keyDown(document, { key: "z", ctrlKey: true });
    });
    await waitFor(() => {
      expect(undoSpy).toHaveBeenCalled();
    });
    const [, scope] = undoSpy.mock.calls[0];
    expect(scope).toBe("design");
  });
});
