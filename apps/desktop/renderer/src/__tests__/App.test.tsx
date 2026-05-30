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
    // Phase 17 Group C Task 19 — these tests render the mode rail
    // and existing mode pages, not the onboarding flow. Persist the
    // dismissed flag up-front so the first-run modal doesn't render
    // on top of Home and double the count of mode labels (the
    // modal's body itself mentions "Design", "Draft", "BIM",
    // "Render" — with the modal open `getByText("Design")` would
    // match the mode rail link AND the modal body, breaking the
    // selector). Onboarding has its own dedicated tests in
    // OnboardingModal.test.tsx.
    window.localStorage.setItem("aec.onboarding.dismissed", "1");
  });

  afterEach(() => {
    // Drop the dismissed flag so other test files in the same Vitest
    // worker that exercise the onboarding flow start from a clean
    // localStorage. Without this, App.test.tsx leaks a persisted "1"
    // into any subsequent suite that does not itself remove the key
    // in its own beforeEach (OnboardingModal.test.tsx already does;
    // future suites might not).
    window.localStorage.removeItem("aec.onboarding.dismissed");
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

// Regression: Devin Review flagged that `undo` / `redo` / `save`
// / `close` in App.tsx listed `project` in their `useCallback`
// dep arrays. Every successful save churns the `project`
// summary (`useActiveProject.saveProject` calls
// `updateProject(summary)` with the refreshed `modifiedAt`),
// which re-derived all four callbacks on every save. The
// shortcut dispatcher itself reads through `cmdRef.current` so
// keybindings still fire, but the callback churn cascades
// through `ActiveProjectProvider`'s `useMemo` context `value`
// identity and forces consumers that list these callbacks as
// deps to refire needlessly. The fix mirrors `project` into a
// ref (`projectRef`) synced via `useEffect`, reads
// `projectRef.current` inside the callbacks, and removes
// `project` from the dep lists. The four callbacks now have
// stable identity across per-save summary updates, matching
// the `projectPathRef` pattern already established in
// `useActiveProject.saveProject` (useActiveProject.tsx:206-218).
//
// Externally observable proof: after a save (which updates
// `project.modifiedAt` in state), Ctrl+Z must still dispatch
// to the bridge with the correct project path. If the ref read
// regressed to a stale closure, the bridge call would either
// fail (project null) or pass an out-of-date path.
describe("App — shortcut callbacks read latest project through ref after save", () => {
  let saveSpy = vi.spyOn(aec.project, "save");
  let undoSpy = vi.spyOn(aec.command, "undo");
  saveSpy.mockRestore();
  undoSpy.mockRestore();

  beforeEach(async () => {
    await ensureProjectOpen();
    saveSpy = vi.spyOn(aec.project, "save");
    undoSpy = vi.spyOn(aec.command, "undo");
  });

  afterEach(() => {
    saveSpy.mockRestore();
    undoSpy.mockRestore();
  });

  it("Ctrl+Z after Ctrl+S still dispatches with the correct project path", async () => {
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });

    // Fire Ctrl+S to trigger saveProject → updateProject(summary)
    // churn (the very condition the ref decouples callbacks
    // from). The save handler resolves synchronously through
    // the in-process backend; we wait for the save spy to
    // observe the call before continuing.
    act(() => {
      fireEvent.keyDown(document, { key: "s", ctrlKey: true });
    });
    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalled();
    });

    // Mock undo to capture the path the callback dispatches
    // with. If the ref read were stale (or projectRef was lost
    // across the post-save re-render), the path would either
    // be undefined or a previous-tick value.
    undoSpy.mockRejectedValue(new Error("nothing to undo"));
    act(() => {
      fireEvent.keyDown(document, { key: "z", ctrlKey: true });
    });
    await waitFor(() => {
      expect(undoSpy).toHaveBeenCalled();
    });
    const [path, scope] = undoSpy.mock.calls[0];
    expect(typeof path).toBe("string");
    expect((path as string).length).toBeGreaterThan(0);
    expect(scope).toBe("design");
  });
});

/**
 * Devin Review (commit 627acbe, finding 3315013227) flagged that
 * App.tsx's `save` callback captures `projectRef.current` BEFORE
 * the `await saveProject()` and announces the success toast with
 * that captured `proj.name` AFTER the await. When the user
 * project-switches mid-save, `saveProject()` still resolves
 * (its internal `projectPathRef` guard skips the state commit
 * but the IIFE itself does NOT reject — the bridge bytes for
 * project A landed on disk, which is the durable contract a
 * save promises). The `addToast("success", \`Saved ${proj.name}\`)`
 * then announces "Saved Project A" against project B's UI.
 *
 * The fix re-reads `projectRef.current` after the save resolves
 * and only toasts when the path still matches the path the save
 * started under. Path equality (not summary reference equality)
 * is used because the push-sync listener may have replaced the
 * summary object even for the same project (e.g. `updateProject`
 * on the save's own success path), so reference comparison would
 * false-negative even when the user did NOT switch projects.
 *
 * This regression test holds `aec.project.save` open across a
 * project transition driven through the real in-process backend
 * (which fires the same push-listener path the production
 * Electron IPC does). The save's mock then resolves after the
 * transition, the `save` callback's post-await `projectRef.current`
 * is project B, the path comparison fails, and the toast is
 * suppressed.
 */
describe("App — save success toast suppressed across mid-save project switch", () => {
  let saveSpy = vi.spyOn(aec.project, "save");
  saveSpy.mockRestore();

  beforeEach(async () => {
    await ensureProjectOpen();
    saveSpy = vi.spyOn(aec.project, "save");
  });

  afterEach(() => {
    saveSpy.mockRestore();
  });

  it("does NOT toast 'Saved <project A>' when the user opens project B while save is in flight", async () => {
    // Hold the save mock open so the test can drive a project
    // transition between dispatch and resolution. The mock
    // returns a fresh summary on resolve so the renderer's
    // post-save `updateProject` call mirrors the production
    // contract (in-process backend's `save` returns the
    // updated row).
    let resolveSave:
      | ((r: {
          projectId: string;
          name: string;
          path: string;
          templateKey: string | null;
          modifiedAt: string;
        }) => void)
      | null = null;
    saveSpy.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSave = resolve;
        }),
    );

    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });

    // Fire Ctrl+S → App.tsx `save` captures `projectRef.current`
    // (project A from `ensureProjectOpen`) and dispatches the
    // save bridge call, which the mock holds open.
    act(() => {
      fireEvent.keyDown(document, { key: "s", ctrlKey: true });
    });
    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalled();
    });

    // While the save is in flight, open project B via the real
    // in-process backend. This fires the push-listener channel
    // (`onActiveProjectChange`) which `useActiveProject`
    // subscribes to on mount; `projectRef.current` becomes
    // project B's summary BEFORE `resolveSave` runs.
    let projectBSummary: {
      projectId: string;
      name: string;
      path: string;
      templateKey: string | null;
      modifiedAt: string;
    } | null = null;
    await act(async () => {
      const created = await aec.project.createFromTemplate(
        "apartment",
        "Project B (race)",
      );
      projectBSummary = created as typeof projectBSummary;
    });
    expect(projectBSummary).not.toBeNull();

    // Resolve the save with project A's marker name. Without
    // the post-await re-check, `addToast("success", \`Saved Project A\`)`
    // would fire on project B's UI. With the fix, the path
    // comparison detects the switch and skips the toast.
    expect(resolveSave).not.toBeNull();
    await act(async () => {
      resolveSave!({
        projectId: "proj_stale_a",
        name: "Project A (stale)",
        path: "/tmp/stale-project-a.aecstudio",
        templateKey: null,
        modifiedAt: new Date().toISOString(),
      });
      // Let the .then() microtask + setState flush.
      await Promise.resolve();
      await Promise.resolve();
    });

    // Critical assertion: the stale "Saved Project A (stale)"
    // success toast must NOT be in the DOM. If the guard
    // regressed, the toast text would appear because the
    // closure-captured `proj.name` (project A's name from the
    // pre-switch capture) would be passed to `addToast`.
    await new Promise((r) => setTimeout(r, 50));
    expect(
      screen.queryByText(/Saved Project A \(stale\)/),
    ).not.toBeInTheDocument();
    // A negative control: error toasts are still allowed to
    // fire (they're project-agnostic in their semantics), so
    // we don't assert "no toast at all" — only the stale-name
    // success toast specifically.
    expect(screen.queryByText(/Save failed/)).not.toBeInTheDocument();
  });

  it("DOES toast 'Saved <name>' when no project switch occurs (positive control)", async () => {
    // Sanity check: the post-await re-check must NOT regress
    // the happy path. When the user saves and stays on the
    // same project, the success toast still fires.
    //
    // We pass through to the real in-process `aec.project.save`
    // (via `vi.spyOn` with no `mockImplementation`) instead of
    // returning a stubbed summary, because the in-process
    // backend's `save` returns a summary whose `path` matches
    // the project's `path` (i.e. `/projects/test_project.aecstudio`
    // — the path generated by `createFromTemplate("apartment", "Test Project")`).
    // A naive `mockResolvedValue({path: "/tmp/different.aecstudio"})`
    // would simulate a project switch (different path) and the
    // guard would correctly suppress the toast — making the
    // mock the bug, not the code. Using the real path lets the
    // happy path actually exercise the toast-firing branch.
    renderAt("/design");
    await waitFor(() => {
      expect(screen.getByTestId("design-mode")).toBeInTheDocument();
    });
    act(() => {
      fireEvent.keyDown(document, { key: "s", ctrlKey: true });
    });
    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalled();
    });
    // Match the toast surfacing — the project name from
    // `ensureProjectOpen` is "Test Project".
    await waitFor(() => {
      expect(screen.getByText(/Saved Test Project/)).toBeInTheDocument();
    });
  });
});
