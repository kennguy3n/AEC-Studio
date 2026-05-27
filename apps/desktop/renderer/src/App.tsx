import { useCallback, useState } from "react";
import { Routes, Route, Navigate, useNavigate } from "react-router-dom";
import { ModeRail } from "./components/ModeRail";
import { StatusBar } from "./components/StatusBar";
import { CommandPalette } from "./components/CommandPalette";
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
} from "./hooks/useToast";
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
  // Navigation shortcuts dismiss any open palette so the user lands on
  // the new route with the overlay cleared.
  const goto = useCallback(
    (path: string) => {
      onClosePalette();
      navigate(path);
    },
    [navigate, onClosePalette],
  );
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
                <Design />
              </RequireProject>
            }
          />
          <Route
            path="/draft"
            element={
              <RequireProject>
                <Draft />
              </RequireProject>
            }
          />
          <Route
            path="/bim"
            element={
              <RequireProject>
                <Bim />
              </RequireProject>
            }
          />
          <Route
            path="/render"
            element={
              <RequireProject>
                <Render />
              </RequireProject>
            }
          />
          <Route
            path="/deliver"
            element={
              <RequireProject>
                <Deliver />
              </RequireProject>
            }
          />
          <Route path="/settings" element={<Settings />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </main>
      <StatusBar />
      <CommandPalette open={paletteOpen} onClose={closePalette} />
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
