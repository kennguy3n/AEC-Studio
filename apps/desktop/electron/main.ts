import { app, BrowserWindow, dialog, session } from "electron";
import * as path from "path";
import { registerIpcHandlers } from "./ipc";
import { registerDialogIpcHandlers } from "./dialog-ipc";
import { installApplicationMenu } from "./menu";
import { onActiveProjectChange } from "./active-project";
import {
  initialiseKchat,
  getKchatDeeplinkBridge,
  shutdownKchat,
} from "./kchat/kchatAppState";
import {
  AEC_PROTOCOL_SCHEME,
  registerProtocolClient,
  DeeplinkBridge,
} from "./kchat/kchatDeeplinkBridge";

let mainWindow: BrowserWindow | null = null;

// Phase 15: claim the single-instance lock BEFORE anything else.
// On Windows + Linux the OS spawns a fresh process for every
// `aecstudio://...` URL the user clicks. The second process
// would otherwise create a duplicate BrowserWindow with its own
// dlopen'd bridge.node, fighting over the SQLite project file
// and the loopback API port. `requestSingleInstanceLock()`
// returns false in the secondary process; we forward our argv
// (where the deeplink URL lives) via the `second-instance` event
// the primary listens to in `kchatDeeplinkBridge.ts`, then exit.
const hasSingleInstanceLock = app.requestSingleInstanceLock();
if (!hasSingleInstanceLock) {
  app.quit();
} else {
  // Register the `aecstudio://` scheme BEFORE `whenReady`. On
  // macOS, Cocoa may dispatch the very first `open-url` event
  // synchronously during `whenReady` resolution, so any later
  // registration would race. The actual deeplink consumer is
  // attached inside `initialiseKchat()` after the renderer
  // window is created.
  registerProtocolClient(app);
}

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

  // Phase 15: once the renderer is up, register a deeplink
  // consumer so any `aecstudio://...` URL the OS hands us
  // reaches the React side. Routes parked during boot (Cocoa
  // delivering `open-url` before `did-finish-load`) are
  // flushed in FIFO order by `setConsumer()` itself.
  mainWindow.webContents.once("did-finish-load", () => {
    const bridge = getKchatDeeplinkBridge();
    if (bridge === null) {
      // `initialiseKchat()` failed during boot — surfaced via
      // its own log line. The window is still useful for every
      // non-KChat surface, so we don't tear it down.
      return;
    }
    bridge.setConsumer((route) => {
      const targetWindow = mainWindow;
      if (targetWindow === null || targetWindow.isDestroyed()) return;
      targetWindow.webContents.send("kchat:deeplink", route);
    });
  });

  // Phase 15: clear the deeplink consumer when the window is
  // destroyed. On macOS the app keeps running after every window
  // closes, so the same bridge instance survives across a close
  // → re-open cycle. Without this handler the consumer still
  // points at the destroyed `mainWindow`: `DeeplinkBridge.dispatch`
  // treats `this.consumer !== null` as "delivered", invokes the
  // closure, the closure short-circuits on `isDestroyed()`, and
  // the route is silently dropped instead of being parked for
  // the next `setConsumer()` call. Clearing the consumer here
  // makes `dispatch` enqueue routes into `this.parked` so the
  // next window flushes them in FIFO order on its own
  // `did-finish-load`.
  mainWindow.on("closed", () => {
    const bridge = getKchatDeeplinkBridge();
    if (bridge !== null) {
      bridge.clearConsumer();
    }
    mainWindow = null;
  });
}

if (hasSingleInstanceLock) {
  app.whenReady().then(async () => {
    // Phase 15: stand up the loopback HTTP API + atomic
    // discovery file + deeplink bridge BEFORE creating the
    // window. The renderer can call `aec.kchat.loopbackStatus`
    // as soon as it loads, so the server has to be bound by
    // then. `initialiseKchat()` is async because the HTTP
    // server's `listen()` is itself async; the rest of the
    // bootstrap is fast.
    try {
      await initialiseKchat();
    } catch (err) {
      // The loopback API failing to bind is a degraded-mode
      // condition (KChat integration unavailable) but not a
      // fatal one — every other surface of AEC Studio still
      // works. Log and continue so the user can keep using
      // the app while the failure mode is diagnosed.
      console.error("[main] initialiseKchat() failed:", err);
    }

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

    // Phase 15: on a `second-instance` event the OS has handed
    // us a new argv from a duplicate launch (typically the
    // user clicking an `aecstudio://...` URL). The deeplink
    // bridge already consumed any matching URL from the argv
    // via the listener attached in `kchatAppState.initialiseKchat`;
    // this handler additionally pops the existing window to
    // the foreground so the user sees the result of their
    // click.
    app.on("second-instance", () => {
      if (mainWindow !== null && !mainWindow.isDestroyed()) {
        if (mainWindow.isMinimized()) mainWindow.restore();
        mainWindow.focus();
      }
    });
  });

  // Phase 15: tear down the loopback API + delete the
  // discovery file before the process exits. `before-quit`
  // fires before `window-all-closed` on macOS during an
  // explicit Cmd+Q, so this is the load-bearing hook. We
  // await `shutdownKchat()` synchronously via
  // `event.preventDefault()` + `app.quit()` after the
  // promise settles so the discovery file is guaranteed to be
  // removed before the next launch reads it.
  let isShuttingDown = false;
  app.on("before-quit", (event) => {
    if (isShuttingDown) return;
    isShuttingDown = true;
    event.preventDefault();
    shutdownKchat()
      .catch((err) => {
        console.error("[main] shutdownKchat() failed:", err);
      })
      .finally(() => {
        app.quit();
      });
  });
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});

// Re-export deeplink bridge symbols for tests. The aliases keep
// the import surface stable if we later move the bridge into a
// different module.
export { DeeplinkBridge, AEC_PROTOCOL_SCHEME };
