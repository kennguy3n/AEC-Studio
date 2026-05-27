import { app, BrowserWindow, dialog, session } from "electron";
import * as path from "path";
import { registerIpcHandlers } from "./ipc";
import { registerDialogIpcHandlers } from "./dialog-ipc";

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
