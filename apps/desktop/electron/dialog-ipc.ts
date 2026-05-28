/**
 * `dialog:openFile` / `dialog:saveFile` / `dialog:openDirectory`
 * IPC handlers.
 *
 * The renderer's project / BIM / Draft / Deliver pages all need a way
 * to surface the OS file picker before they invoke the bridge — e.g.
 * "Import IFC" needs the user-selected IFC path before calling
 * `aec.bim.importIfc`, and "Export PDF" needs a save destination
 * before calling `aec.export.exportPdf`. Until Phase 13 the renderer
 * passed `"demo://..."` placeholders into the bridge; this module is
 * the production file-picker surface that replaces them.
 *
 * Why a dedicated module instead of inlining the calls in `ipc.ts`?
 *   1. The handlers depend on a `BrowserWindow` reference (so the
 *      modal is parented to the correct window) but `ipc.ts` is
 *      window-agnostic and runs before any window exists. Bundling
 *      the handlers here keeps the window-injection wiring explicit.
 *   2. The shape of `OpenFileFilters` / `SaveDialogOptions` is shared
 *      between several callers (BIM import, Draft DXF import, Deliver
 *      export). Centralising the input validation here keeps every
 *      handler's preconditions identical.
 *   3. Tests stub `dialog.showOpenDialog` / `dialog.showSaveDialog`
 *      via the exported `__setDialogModule` test hook, so a unit test
 *      can drive the IPC path without spinning up Electron.
 */

import { ipcMain, type BrowserWindow, type FileFilter } from "electron";

/** Real Electron dialog surface, swapped for a stub in tests. */
interface DialogModule {
  showOpenDialog: (
    window: BrowserWindow | null,
    options: {
      title?: string;
      defaultPath?: string;
      filters?: FileFilter[];
      properties?: Array<
        | "openFile"
        | "openDirectory"
        | "multiSelections"
        | "showHiddenFiles"
        | "createDirectory"
        | "promptToCreate"
        | "noResolveAliases"
        | "treatPackageAsDirectory"
        | "dontAddToRecent"
      >;
      message?: string;
    },
  ) => Promise<{ canceled: boolean; filePaths: string[] }>;
  showSaveDialog: (
    window: BrowserWindow | null,
    options: {
      title?: string;
      defaultPath?: string;
      filters?: FileFilter[];
      message?: string;
    },
  ) => Promise<{ canceled: boolean; filePath?: string }>;
}

let dialogModule: DialogModule | null = null;
let getActiveWindow: () => BrowserWindow | null = () => null;

/**
 * Bind the IPC handlers to the live `electron.dialog` module and a
 * function returning the parent window for any modal we show. Called
 * once from `main.ts` after the first `BrowserWindow` is created.
 *
 * Separating the binding from registration lets the unit tests swap
 * in a stub dialog module via `__setDialogModule`.
 */
export function registerDialogIpcHandlers(opts: {
  dialog: DialogModule;
  getActiveWindow: () => BrowserWindow | null;
}): void {
  dialogModule = opts.dialog;
  getActiveWindow = opts.getActiveWindow;

  ipcMain.handle("dialog:openFile", async (_e, raw) => {
    const params = normaliseOpenFileParams(raw);
    const res = await dialogModule!.showOpenDialog(getActiveWindow(), {
      title: params.title,
      defaultPath: params.defaultPath,
      filters: params.filters,
      message: params.message,
      properties: params.allowMultiple
        ? ["openFile", "multiSelections"]
        : ["openFile"],
    });
    if (res.canceled || res.filePaths.length === 0) {
      return { canceled: true, paths: [] as string[] };
    }
    return { canceled: false, paths: res.filePaths };
  });

  ipcMain.handle("dialog:openDirectory", async (_e, raw) => {
    const params = normaliseOpenDirectoryParams(raw);
    const res = await dialogModule!.showOpenDialog(getActiveWindow(), {
      title: params.title,
      defaultPath: params.defaultPath,
      message: params.message,
      properties: ["openDirectory"],
    });
    if (res.canceled || res.filePaths.length === 0) {
      return { canceled: true, path: null };
    }
    return { canceled: false, path: res.filePaths[0] };
  });

  ipcMain.handle("dialog:saveFile", async (_e, raw) => {
    const params = normaliseSaveFileParams(raw);
    const res = await dialogModule!.showSaveDialog(getActiveWindow(), {
      title: params.title,
      defaultPath: params.defaultPath,
      filters: params.filters,
      message: params.message,
    });
    if (res.canceled || !res.filePath) {
      return { canceled: true, path: null };
    }
    return { canceled: false, path: res.filePath };
  });
}

export interface OpenFileParams {
  title?: string;
  defaultPath?: string;
  filters?: FileFilter[];
  message?: string;
  allowMultiple?: boolean;
}

export interface OpenDirectoryParams {
  title?: string;
  defaultPath?: string;
  message?: string;
}

export interface SaveFileParams {
  title?: string;
  defaultPath?: string;
  filters?: FileFilter[];
  message?: string;
}

function normaliseOpenFileParams(raw: unknown): OpenFileParams {
  const obj = ensureObject(raw, "dialog:openFile");
  return {
    title: optionalString(obj.title, "title", "dialog:openFile"),
    defaultPath: optionalString(
      obj.defaultPath,
      "defaultPath",
      "dialog:openFile",
    ),
    filters: optionalFilters(obj.filters, "dialog:openFile"),
    message: optionalString(obj.message, "message", "dialog:openFile"),
    allowMultiple:
      typeof obj.allowMultiple === "boolean" ? obj.allowMultiple : false,
  };
}

function normaliseOpenDirectoryParams(raw: unknown): OpenDirectoryParams {
  const obj = ensureObject(raw, "dialog:openDirectory");
  return {
    title: optionalString(obj.title, "title", "dialog:openDirectory"),
    defaultPath: optionalString(
      obj.defaultPath,
      "defaultPath",
      "dialog:openDirectory",
    ),
    message: optionalString(obj.message, "message", "dialog:openDirectory"),
  };
}

function normaliseSaveFileParams(raw: unknown): SaveFileParams {
  const obj = ensureObject(raw, "dialog:saveFile");
  return {
    title: optionalString(obj.title, "title", "dialog:saveFile"),
    defaultPath: optionalString(
      obj.defaultPath,
      "defaultPath",
      "dialog:saveFile",
    ),
    filters: optionalFilters(obj.filters, "dialog:saveFile"),
    message: optionalString(obj.message, "message", "dialog:saveFile"),
  };
}

function ensureObject(value: unknown, method: string): Record<string, unknown> {
  if (value === null || value === undefined) {
    // `aec.dialog.*()` callers may pass no params at all when they
    // want the OS default behaviour ("any file", no filters). Accept
    // that as an empty object rather than throwing.
    return {};
  }
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${method}: params must be an object`);
  }
  return value as Record<string, unknown>;
}

function optionalString(
  v: unknown,
  field: string,
  method: string,
): string | undefined {
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "string") {
    throw new Error(`${method}: '${field}' must be a string when present`);
  }
  return v;
}

function optionalFilters(
  v: unknown,
  method: string,
): FileFilter[] | undefined {
  if (v === undefined || v === null) return undefined;
  if (!Array.isArray(v)) {
    throw new Error(`${method}: 'filters' must be an array when present`);
  }
  return v.map((entry, i) => {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      throw new Error(
        `${method}: 'filters[${i}]' must be an object with name + extensions`,
      );
    }
    const obj = entry as Record<string, unknown>;
    if (typeof obj.name !== "string" || obj.name.length === 0) {
      throw new Error(
        `${method}: 'filters[${i}].name' must be a non-empty string`,
      );
    }
    if (!Array.isArray(obj.extensions)) {
      throw new Error(`${method}: 'filters[${i}].extensions' must be an array`);
    }
    const extensions = obj.extensions.map((e, j) => {
      if (typeof e !== "string" || e.length === 0) {
        throw new Error(
          `${method}: 'filters[${i}].extensions[${j}]' must be a non-empty string`,
        );
      }
      return e;
    });
    return { name: obj.name, extensions };
  });
}

/**
 * Test hook: replace the live dialog module with a stub so the unit
 * tests can drive the IPC path without spinning up Electron.
 *
 * Not exported through the public IPC surface — tests import this
 * directly and reset the module between cases via
 * `__resetDialogModule()`.
 */
export function __setDialogModule(
  dialog: DialogModule,
  windowGetter: () => BrowserWindow | null = () => null,
): void {
  dialogModule = dialog;
  getActiveWindow = windowGetter;
}

/** Test hook: clear the registered dialog module + window getter. */
export function __resetDialogModule(): void {
  dialogModule = null;
  getActiveWindow = () => null;
}
