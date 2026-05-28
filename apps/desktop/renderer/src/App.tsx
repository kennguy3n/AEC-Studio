import { useCallback, useEffect, useState } from "react";
import { Routes, Route, Navigate, useLocation, useNavigate } from "react-router-dom";
import { ModeRail } from "./components/ModeRail";
import { StatusBar } from "./components/StatusBar";
import { CommandPalette } from "./components/CommandPalette";
import { ShortcutHelp } from "./components/ShortcutHelp";
import { ErrorBoundary } from "./components/ErrorBoundary";
import {
  useKeyboardShortcuts,
  useShortcut,
} from "./hooks/useKeyboardShortcuts";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "./hooks/useActiveProject";
import {
  ToastProvider,
  ToastContainer,
  useToast,
} from "./hooks/useToast";
import { aec } from "./api/aec";
import type { CommandScope } from "./api/commands";
import { Home } from "./pages/Home";
import { Design } from "./pages/Design";
import { Draft } from "./pages/Draft";
import { Bim } from "./pages/Bim";
import { Render } from "./pages/Render";
import { Deliver } from "./pages/Deliver";
import { Settings } from "./pages/Settings";

function RequireProject({ children }: { children: JSX.Element }): JSX.Element {
  const { project, loading } = useActiveProject();
  if (loading) {
    return (
      <div className="loading-gate" data-testid="loading-gate">
        Loading…
      </div>
    );
  }
  if (project === null) {
    return <Navigate to="/" replace />;
  }
  return children;
}

function AppCommands({
  onOpenPalette,
  onClosePalette,
}: {
  onOpenPalette: () => void;
  onClosePalette: () => void;
}): null {
  const navigate = useNavigate();
  const location = useLocation();
  const {
    saveProject,
    closeProject,
    setUndoRedo,
    markDirty,
    getActiveProject,
  } = useActiveProject();
  const { addToast } = useToast();

  // The four shortcut callbacks below (undo / redo / save / close)
  // need to read the latest active project inside async handlers,
  // but listing `project` in their `useCallback` dep arrays would
  // churn callback identity on every successful save —
  // `useActiveProject.saveProject` calls `updateProject(summary)`
  // after each save to surface the refreshed `modifiedAt`, which
  // creates a fresh `ProjectSummary` reference and forces React to
  // re-derive `undo` / `redo` / `save` / `close`. The same churn
  // happens on every `project:active-changed` push from the main
  // process (e.g. multi-window or test harnesses mutating the
  // tracker directly). Those new closures then cascade through the
  // `useMemo` context `value` identity in `ActiveProjectProvider`,
  // which would re-render every consumer that depends on the
  // context on every 5s auto-save even when the fields they
  // consume have not changed.
  //
  // The shortcut dispatcher (`useShortcut` → `cmdRef.current` in
  // `useKeyboardShortcuts.ts:153`) already reads the latest handler
  // through a ref on every keypress, so the keybindings always fire
  // the current callback regardless of identity churn — but child
  // components or effects that list these callbacks as deps would
  // refire needlessly. Reading the active project through
  // `getActiveProject()` (a stable getter exposed by
  // `useActiveProject` that reads the central synchronous
  // `projectSummaryRef`) decouples callback identity from per-save
  // summary updates without any per-component ref mirroring.
  //
  // The previous incarnation of this file maintained its own
  // `projectRef + useEffect` sync; Devin Review flagged the
  // resulting one-render-cycle lag versus `useActiveProject`'s
  // internal sync ref (see the `getActiveProject` docblock on
  // `ActiveProjectState`). Routing through the central getter
  // closes that gap by structural construction so the save toast
  // sees the same project name the bridge wrote, even when a
  // project transition races the in-flight save.

  // The command engine partitions undo/redo by mode scope; map the
  // current route to the matching scope so Ctrl/Cmd+Z undoes within
  // the active mode. Unknown routes (Home, Settings, etc.) resolve
  // to `null` so the shortcut becomes a silent no-op instead of
  // mis-attributing the keystroke to the "design" scope. The previous
  // "fall back to design" behaviour was a bug: a user who placed a
  // chair in Design, navigated to Home (project still open), and
  // reflexively hit Ctrl/Cmd+Z would silently undo that placement
  // from a screen with no visual affordance for the action. The
  // `proj === null` guard inside each callback catches the
  // no-project-open case; this `activeScope === null` guard catches
  // the project-open-but-not-in-a-mode case.
  const activeScope: CommandScope | null = (() => {
    const path = location.pathname.split("/")[1] ?? "";
    if (path === "design" || path === "draft" || path === "bim" ||
        path === "render" || path === "deliver") {
      return path;
    }
    return null;
  })();
  // Navigation shortcuts dismiss any open palette so the user lands on
  // the new route with the overlay cleared.
  const goto = useCallback(
    (path: string) => {
      onClosePalette();
      navigate(path);
    },
    [navigate, onClosePalette],
  );

  // Undo / Redo route through the main process which delegates to the
  // bridge's command engine. The renderer keeps the undo/redo stack
  // depths in sync via `setUndoRedo` so the StatusBar reflects the
  // current state.
  //
  // Reads project state through `getActiveProject()` (the central
  // sync getter from `useActiveProject`) so the callback identity
  // stays stable across per-save summary updates. `activeScope` is
  // derived from `useLocation()` and only changes on navigation, so
  // it stays in the dep array.
  const undo = useCallback(async () => {
    const proj = getActiveProject();
    if (proj === null || activeScope === null) return;
    try {
      const result = await aec.command.undo(proj.path, activeScope);
      setUndoRedo(result.undoLen, result.redoLen);
      markDirty();
    } catch (err) {
      addToast(
        "error",
        `Undo failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [activeScope, setUndoRedo, markDirty, addToast, getActiveProject]);

  const redo = useCallback(async () => {
    const proj = getActiveProject();
    if (proj === null || activeScope === null) return;
    try {
      const result = await aec.command.redo(proj.path, activeScope);
      setUndoRedo(result.undoLen, result.redoLen);
      markDirty();
    } catch (err) {
      addToast(
        "error",
        `Redo failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [activeScope, setUndoRedo, markDirty, addToast, getActiveProject]);

  const save = useCallback(async () => {
    const proj = getActiveProject();
    if (proj === null) return;
    try {
      await saveProject();
      // Re-check the active project after the await before
      // surfacing the success toast. `saveProject()` internally
      // guards its state commit on the same path (see the
      // `projectPathRef.current === savePath` check in
      // `useActiveProject.saveProject`'s inflight IIFE), so when
      // a project transition (open / create / close / switch)
      // runs mid-save, the promise still RESOLVES — the bytes
      // landed on disk for project A, which is the durable
      // contract a save promises — but the renderer is now on
      // project B. Firing `addToast("success", `Saved ${proj.name}`)`
      // unconditionally would announce "Saved Project A" against
      // project B's UI, which is the toast-text analogue of the
      // same race we already close inside the hook for state
      // commits. Skipping the toast on path mismatch lets the
      // disk write stay (correct) while declining to announce
      // the wrong project name (also correct).
      //
      // We compare the path, not the summary reference: the push-
      // sync listener may have replaced the summary object even
      // for the same project (e.g. on the very save that just
      // completed), so reference equality would false-negative.
      // Path equality is the stable identity used everywhere else
      // in the hook for the same reason. `getActiveProject()`
      // reads the central sync ref, so a project switch that
      // landed in the microtask between the bridge resolution and
      // this check is observed immediately — the previous
      // `projectRef` + `useEffect` pattern had a one-render-cycle
      // lag here that could let project A's name toast on
      // project B's UI.
      const current = getActiveProject();
      if (current !== null && current.path === proj.path) {
        addToast("success", `Saved ${proj.name}`);
      }
    } catch (err) {
      addToast(
        "error",
        `Save failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [saveProject, addToast, getActiveProject]);

  const close = useCallback(async () => {
    if (getActiveProject() === null) return;
    // Wrap the bridge call in try/catch to match the convention
    // used by the sibling `save` / `undo` / `redo` callbacks.
    // Today the main process's `project:close` handler only calls
    // `clearActiveProjectPath()` (synchronous, can't fail), so an
    // unhandled rejection here is unreachable. But the handler
    // signature is async — and any future enhancement (a
    // `flushPendingChanges` await per the deferred design
    // tracker, an "are you sure?" confirmation dialog, telemetry
    // emission) will create a real failure surface. Without this
    // catch the bridge error would bubble as an unhandled promise
    // rejection, and `navigate("/")` would never run — leaving
    // the user stuck on the now-broken project view with no
    // visible error. The toast follows the same `${err.message}`
    // shape as the other handlers so the message is grep-able
    // across the codebase, and we still attempt the navigation
    // on failure so a partial close (project state may already be
    // cleared on the renderer) doesn't strand the user.
    try {
      await closeProject();
    } catch (err) {
      addToast(
        "error",
        `Close failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
    navigate("/");
  }, [closeProject, navigate, addToast, getActiveProject]);

  useShortcut({
    id: "undo",
    label: "Undo",
    group: "global",
    keys: "mod+z",
    handler: () => void undo(),
  });
  useShortcut({
    id: "redo",
    label: "Redo",
    group: "global",
    keys: "mod+shift+z",
    handler: () => void redo(),
  });
  useShortcut({
    id: "save",
    label: "Save project",
    group: "global",
    keys: "mod+s",
    handler: () => void save(),
  });
  useShortcut({
    id: "close-project",
    label: "Close project",
    group: "global",
    keys: "mod+w",
    handler: () => void close(),
  });
  useShortcut({
    id: "open-command-palette",
    label: "Open command palette",
    group: "global",
    keys: "mod+k",
    handler: onOpenPalette,
    whenInputFocused: true,
  });
  useShortcut({
    id: "goto-home",
    label: "Go to Home",
    group: "navigation",
    keys: "mod+1",
    handler: () => goto("/"),
  });
  useShortcut({
    id: "goto-design",
    label: "Go to Design",
    group: "navigation",
    keys: "mod+2",
    handler: () => goto("/design"),
  });
  useShortcut({
    id: "goto-draft",
    label: "Go to Draft",
    group: "navigation",
    keys: "mod+3",
    handler: () => goto("/draft"),
  });
  useShortcut({
    id: "goto-bim",
    label: "Go to BIM",
    group: "navigation",
    keys: "mod+4",
    handler: () => goto("/bim"),
  });
  useShortcut({
    id: "goto-render",
    label: "Go to Render",
    group: "navigation",
    keys: "mod+5",
    handler: () => goto("/render"),
  });
  useShortcut({
    id: "goto-deliver",
    label: "Go to Deliver",
    group: "navigation",
    keys: "mod+6",
    handler: () => goto("/deliver"),
  });
  useShortcut({
    id: "goto-settings",
    label: "Go to Settings",
    group: "navigation",
    keys: "mod+,",
    handler: () => goto("/settings"),
  });
  return null;
}

function AppShell() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const openPalette = useCallback(() => setPaletteOpen(true), []);
  const closePalette = useCallback(() => setPaletteOpen(false), []);
  const { project } = useActiveProject();

  useKeyboardShortcuts();

  return (
    <div className="app-shell">
      <ModeRail />
      <main className="app-main">
        {project && (
          <div
            className="app-project-header"
            data-testid="project-header"
          >
            {project.name}
          </div>
        )}
        <AppCommands onOpenPalette={openPalette} onClosePalette={closePalette} />
        <Routes>
          <Route path="/" element={<Home />} />
          <Route
            path="/design"
            element={
              <RequireProject>
                <ErrorBoundary label="Design">
                  <Design />
                </ErrorBoundary>
              </RequireProject>
            }
          />
          <Route
            path="/draft"
            element={
              <RequireProject>
                <ErrorBoundary label="Draft">
                  <Draft />
                </ErrorBoundary>
              </RequireProject>
            }
          />
          <Route
            path="/bim"
            element={
              <RequireProject>
                <ErrorBoundary label="BIM">
                  <Bim />
                </ErrorBoundary>
              </RequireProject>
            }
          />
          <Route
            path="/render"
            element={
              <RequireProject>
                <ErrorBoundary label="Render">
                  <Render />
                </ErrorBoundary>
              </RequireProject>
            }
          />
          <Route
            path="/deliver"
            element={
              <RequireProject>
                <ErrorBoundary label="Deliver">
                  <Deliver />
                </ErrorBoundary>
              </RequireProject>
            }
          />
          <Route
            path="/settings"
            element={
              <ErrorBoundary label="Settings">
                <Settings />
              </ErrorBoundary>
            }
          />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </main>
      <StatusBar />
      <CommandPalette open={paletteOpen} onClose={closePalette} />
      <ShortcutHelp />
      <ToastContainer />
    </div>
  );
}

export default function App() {
  return (
    <ToastProvider>
      <ActiveProjectProvider>
        <AppShell />
      </ActiveProjectProvider>
    </ToastProvider>
  );
}
