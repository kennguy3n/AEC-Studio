import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";
import App from "./App";
import "./styles/tokens.css";
import "./styles/global.css";
import "./styles/components.css";
import { applyThemeMode, readStoredThemeMode } from "./lib/theme";

// Phase 17 Task 8 — apply the persisted theme before React renders.
// This runs synchronously inside the import graph, so the very first
// paint already has the right `data-theme` attribute on <html>; there
// is no light→dark flash on a dark-preferring user's reload.
applyThemeMode(readStoredThemeMode());

// HashRouter (not BrowserRouter) is the correct router for an Electron
// renderer that loads its HTML via `file://`. `BrowserRouter` reads
// `window.location.pathname`, which under `file://` is the absolute path
// to `index.html` (e.g. `/Users/.../dist/index.html`) — that never matches
// any defined route, so the catch-all `<Navigate to="/" replace />` fires
// on every load and a hard refresh on `/design` cannot reach the deep
// link. `HashRouter` keeps the route in the URL fragment, which works
// identically over `file://` and `http://`.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <HashRouter>
      <App />
    </HashRouter>
  </React.StrictMode>,
);
