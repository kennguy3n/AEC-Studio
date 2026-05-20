import { useCallback, useEffect, useState } from "react";
import { Routes, Route, Navigate, useNavigate } from "react-router-dom";
import { ModeRail } from "./components/ModeRail";
import { StatusBar } from "./components/StatusBar";
import { CommandPalette } from "./components/CommandPalette";
import {
  useKeyboardShortcuts,
  useShortcut,
} from "./hooks/useKeyboardShortcuts";
import { Home } from "./pages/Home";
import { Design } from "./pages/Design";
import { Draft } from "./pages/Draft";
import { Bim } from "./pages/Bim";
import { Render } from "./pages/Render";
import { Deliver } from "./pages/Deliver";
import { Settings } from "./pages/Settings";

function AppCommands({
  onOpenPalette,
}: {
  onOpenPalette: () => void;
}): null {
  const navigate = useNavigate();
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
    handler: () => navigate("/"),
  });
  useShortcut({
    id: "goto-design",
    label: "Go to Design",
    group: "navigation",
    keys: "mod+2",
    handler: () => navigate("/design"),
  });
  useShortcut({
    id: "goto-draft",
    label: "Go to Draft",
    group: "navigation",
    keys: "mod+3",
    handler: () => navigate("/draft"),
  });
  useShortcut({
    id: "goto-bim",
    label: "Go to BIM",
    group: "navigation",
    keys: "mod+4",
    handler: () => navigate("/bim"),
  });
  useShortcut({
    id: "goto-render",
    label: "Go to Render",
    group: "navigation",
    keys: "mod+5",
    handler: () => navigate("/render"),
  });
  useShortcut({
    id: "goto-deliver",
    label: "Go to Deliver",
    group: "navigation",
    keys: "mod+6",
    handler: () => navigate("/deliver"),
  });
  useShortcut({
    id: "goto-settings",
    label: "Go to Settings",
    group: "navigation",
    keys: "mod+,",
    handler: () => navigate("/settings"),
  });
  return null;
}

export default function App() {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const openPalette = useCallback(() => setPaletteOpen(true), []);
  const closePalette = useCallback(() => setPaletteOpen(false), []);

  useKeyboardShortcuts();

  // Close palette on route changes from non-shortcut sources (e.g.
  // mouse clicks). The Escape key is handled inside the palette
  // itself.
  useEffect(() => {
    if (!paletteOpen) return;
    const onMouseUp = () => {};
    window.addEventListener("mouseup", onMouseUp);
    return () => window.removeEventListener("mouseup", onMouseUp);
  }, [paletteOpen]);

  return (
    <div className="app-shell">
      <ModeRail />
      <main className="app-main">
        <AppCommands onOpenPalette={openPalette} />
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/design" element={<Design />} />
          <Route path="/draft" element={<Draft />} />
          <Route path="/bim" element={<Bim />} />
          <Route path="/render" element={<Render />} />
          <Route path="/deliver" element={<Deliver />} />
          <Route path="/settings" element={<Settings />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </main>
      <StatusBar />
      <CommandPalette open={paletteOpen} onClose={closePalette} />
    </div>
  );
}
