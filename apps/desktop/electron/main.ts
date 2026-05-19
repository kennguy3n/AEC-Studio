import { app, BrowserWindow, session } from "electron";
import * as path from "path";
import { registerIpcHandlers } from "./ipc";

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
