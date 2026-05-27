import { app, BrowserWindow, dialog, session } from "electron";
import * as path from "path";
import { registerIpcHandlers } from "./ipc";
import { registerDialogIpcHandlers } from "./dialog-ipc";
import { installApplicationMenu } from "./menu";
import { onActiveProjectChange } from "./active-project";

let mainWindow: BrowserWindow | null = null;

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 1100,
    minHeight: 700,
    title: "AEC Studio",
    backgroundColor: "#F8F7FB",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  const isDev = !app.isPackaged;
  // Build a Content-Security-Policy whose *production* form drops every
  // dev-only relaxation:
  //   * `style-src 'unsafe-inline'` is needed for Vite's HMR style
  //     injection in dev, but a production Vite build emits external
  //     `.css` files, so 'self' alone is sufficient.
  //   * `https://fonts.googleapis.com` / `https://fonts.gstatic.com` are
  //     only ever loaded by the Vite dev server scaffolding; the app
  //     ships Inter as a local font-family fallback.
  //   * `ws://localhost:5173` / `http://localhost:5173` are the Vite
  //     dev HMR endpoints.
  // Keeping the dev policy loose-but-explicit lets us iterate quickly
  // while the production policy stays as strict as possible.
  const styleSrc = isDev
    ? "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com"
    : "style-src 'self'";
  const fontSrc = isDev ? "font-src 'self' https://fonts.gstatic.com" : "font-src 'self'";
  const connectSrc = isDev
    ? "connect-src 'self' ws://localhost:5173 http://localhost:5173"
    : "connect-src 'self'";

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: {
        ...details.responseHeaders,
        "Content-Security-Policy": [
          [
            "default-src 'self'",
            "script-src 'self'",
            styleSrc,
            fontSrc,
            "img-src 'self' data:",
            connectSrc,
          ].join("; "),
        ],
      },
    });
  });

  if (isDev) {
    mainWindow.loadURL("http://localhost:5173");
  } else {
    mainWindow.loadFile(path.join(__dirname, "../dist/index.html"));
  }
}

app.whenReady().then(() => {
  // Install the application menu BEFORE creating the window. On
  // macOS the default Electron menu binds `CmdOrCtrl+W` to "Close
  // Window" via the standard `windowMenu` role; because Electron
  // processes menu accelerators before keydown events reach the
  // renderer, the renderer's `mod+w` "Close project" shortcut
  // (App.tsx) would never fire — Cmd+W would destroy the
  // BrowserWindow instead of closing just the project. On
  // Windows/Linux there is no native menu-bar convention worth
  // keeping; suppressing the default menu sends every shortcut
  // through the renderer's command palette / `useShortcut`
  // registry. See `menu.ts` for the full rationale.
  installApplicationMenu();
  registerIpcHandlers();
  // Dialog IPC handlers (`dialog:openFile`, `dialog:openDirectory`,
  // `dialog:saveFile`) live in a separate module so the renderer-side
  // file-picker calls don't get rejected because the channels aren't
  // registered on the main process. Without this, every `aec.dialog.*`
  // call (BIM Import IFC, Draft DXF import/export, Deliver Build Pack
  // save dialog, Home Open Project directory picker) would silently
  // fail in production — Vitest hides the bug because the in-process
  // renderer backend short-circuits with `canceled: true`.
  //
  // `mainWindow` is `null` here (we register before `createWindow()`)
  // but the handlers re-evaluate `getActiveWindow()` on every IPC
  // call, so by the time the user clicks "Import" the BrowserWindow
  // reference is live. Registering early keeps the channels available
  // for any renderer that loads before the window is shown.
  // Electron's `dialog.showOpenDialog` / `showSaveDialog` are overloaded
  // — one form takes `(BrowserWindow, options)` (modal/parented), the
  // other `(options)` (un-parented). Our `DialogModule` contract takes
  // `BrowserWindow | null`; this adapter dispatches to the correct
  // overload so a null parent doesn't crash the type checker.
  registerDialogIpcHandlers({
    dialog: {
      showOpenDialog: (window, options) =>
        window === null
          ? dialog.showOpenDialog(options)
          : dialog.showOpenDialog(window, options),
      showSaveDialog: (window, options) =>
        window === null
          ? dialog.showSaveDialog(options)
          : dialog.showSaveDialog(window, options),
    },
    getActiveWindow: () => mainWindow,
  });
  // Forward active-project changes from the main-process tracker
  // (`active-project.ts → onActiveProjectChange`) to every renderer
  // window via the `project:active-changed` IPC channel. The
  // renderer's `useActiveProject` hook subscribes via
  // `aec.project.onActiveProjectChange(...)` and updates its local
  // state on every push. Today there is a single window so the push
  // is a defensive no-op for any change that originated from the same
  // window's own bridge call (the renderer already updated state
  // before the push round-tripped back); when multi-window support
  // is added — a second BrowserWindow for "Open project in new
  // window", Print Preview, pop-out viewport — every window stays in
  // lockstep on which project is active without polling.
  //
  // `BrowserWindow.getAllWindows()` is evaluated on every notification
  // so windows opened after this subscription is registered still
  // receive pushes; destroyed/closed windows are filtered out via
  // `isDestroyed()` to avoid sending to a freed `webContents` (which
  // would log a warning and is a leak target in long-running sessions
  // that open/close windows repeatedly).
  onActiveProjectChange((summary) => {
    for (const window of BrowserWindow.getAllWindows()) {
      if (window.isDestroyed()) {
        continue;
      }
      window.webContents.send("project:active-changed", summary);
    }
  });
  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
