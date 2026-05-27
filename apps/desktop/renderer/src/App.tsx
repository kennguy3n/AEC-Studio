import { useCallback, useState } from "react";
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
  const { project, saveProject, closeProject, setUndoRedo, markDirty } =
    useActiveProject();
  const { addToast } = useToast();

  // The command engine partitions undo/redo by mode scope; map the
  // current route to the matching scope so Ctrl/Cmd+Z undoes within
  // the active mode. Unknown routes (Home, Settings, etc.) resolve
  // to `null` so the shortcut becomes a silent no-op instead of
  // mis-attributing the keystroke to the "design" scope. The previous
  // "fall back to design" behaviour was a bug: a user who placed a
  // chair in Design, navigated to Home (project still open), and
  // reflexively hit Ctrl/Cmd+Z would silently undo that placement
  // from a screen with no visual affordance for the action. The
  // `project === null` guard below catches the no-project-open case;
  // this `activeScope === null` guard catches the
  // project-open-but-not-in-a-mode case.
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
  const undo = useCallback(async () => {
    if (project === null || activeScope === null) return;
    try {
      const result = await aec.command.undo(project.path, activeScope);
      setUndoRedo(result.undoLen, result.redoLen);
      markDirty();
    } catch (err) {
      addToast(
        "error",
        `Undo failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [project, activeScope, setUndoRedo, markDirty, addToast]);

  const redo = useCallback(async () => {
    if (project === null || activeScope === null) return;
    try {
      const result = await aec.command.redo(project.path, activeScope);
      setUndoRedo(result.undoLen, result.redoLen);
      markDirty();
    } catch (err) {
      addToast(
        "error",
        `Redo failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [project, activeScope, setUndoRedo, markDirty, addToast]);

  const save = useCallback(async () => {
    if (project === null) return;
    try {
      await saveProject();
      addToast("success", `Saved ${project.name}`);
    } catch (err) {
      addToast(
        "error",
        `Save failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }, [project, saveProject, addToast]);

  const close = useCallback(async () => {
    if (project === null) return;
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
  }, [project, closeProject, navigate, addToast]);

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
