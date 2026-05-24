/**
 * Size-guarded BIM/IFC import helper.
 *
 * The bridge's `bim_import_ifc` reads the whole file into memory and
 * runs the STEP-21 parse synchronously on the calling thread. On a
 * 400 MB MEP federation that's several seconds of wall-time during
 * which the renderer can't respond. Rather than surprise the user
 * with the wait, the renderer calls `aec.bim.checkFileSize(path)`
 * first — one `fs::metadata` syscall on the native side — and shows
 * a confirm dialog if the file is at or above the 100 MB threshold.
 * Only then does it commit to the parse path via `aec.bim.importIfc`.
 *
 * The helper is a pure function over a confirm callback so it can be
 * unit-tested without spinning up Electron dialog infrastructure.
 * Production callers pass `window.confirm`; tests pass a stub.
 */

import { aec } from "./aec";

/**
 * Result of a size-guarded import attempt. `kind` discriminates so
 * callers can render the right toast / status line:
 *
 *   * `"imported"` — file was parsed and the bridge returned a
 *     summary; payload is the bridge result verbatim.
 *   * `"cancelled-by-user"` — user declined the large-file confirm
 *     dialog. The renderer should restore the previous state and
 *     leave a "import cancelled" hint visible.
 *   * `"failed"` — the bridge returned an error (file missing,
 *     permission denied, malformed IFC, …). `error` is the raw
 *     error message so the renderer can surface it in a toast.
 */
export type BimImportOutcome =
  | { kind: "imported"; result: unknown }
  | { kind: "cancelled-by-user"; sizeBytes: number; thresholdBytes: number }
  | { kind: "failed"; error: string };

/**
 * Callback invoked when a file's size meets-or-exceeds the warn
 * threshold. Implementations should show a confirm dialog and
 * resolve to `true` to proceed with the parse, `false` to abort.
 *
 * In production the default callback is a thin wrapper around
 * `window.confirm`; tests pass a stub so the dialog is observable.
 */
export type LargeFileConfirm = (info: {
  path: string;
  sizeBytes: number;
  thresholdBytes: number;
}) => boolean | Promise<boolean>;

/**
 * Default confirm callback: renders a human-readable "X MB / Y MB"
 * line via `window.confirm`. Kept here (not inlined in the caller)
 * so the message string is exercised by the unit test and won't
 * drift from the i18n keys we'll add in a future PR.
 */
export const defaultLargeFileConfirm: LargeFileConfirm = ({
  path,
  sizeBytes,
  thresholdBytes,
}) => {
  const sizeMb = (sizeBytes / (1024 * 1024)).toFixed(1);
  const thresholdMb = (thresholdBytes / (1024 * 1024)).toFixed(0);
  const message =
    `The IFC file is ${sizeMb} MB, above the ${thresholdMb} MB warn ` +
    `threshold:\n\n${path}\n\n` +
    `Parsing may take a while and the application will be ` +
    `unresponsive during the parse. Continue?`;
  if (typeof window !== "undefined" && typeof window.confirm === "function") {
    return window.confirm(message);
  }
  // No window.confirm available (SSR / Jest jsdom without confirm
  // stub). Default to proceeding so we never silently swallow an
  // import the caller intended; the confirm is an *advisory* UX
  // affordance, not a hard guard.
  return true;
};

/**
 * Run a size-guarded IFC import.
 *
 * Calling sequence:
 *   1. `aec.bim.checkFileSize(path)` (cheap; one stat syscall).
 *   2. If `largeFileWarning`, invoke `onLargeFile`. If it resolves
 *      to `false`, return `{ kind: "cancelled-by-user", ... }`
 *      without invoking the parse.
 *   3. Otherwise call `aec.bim.importIfc(path)` and return
 *      `{ kind: "imported", result }`.
 *
 * Errors from either bridge call are caught and returned as
 * `{ kind: "failed", error }` rather than thrown — the renderer
 * UX layer wants to render a toast, not crash an error boundary.
 */
export async function importIfcWithSizeGuard(
  path: string,
  onLargeFile: LargeFileConfirm = defaultLargeFileConfirm,
): Promise<BimImportOutcome> {
  let check;
  try {
    check = await aec.bim.checkFileSize(path);
  } catch (err) {
    return { kind: "failed", error: errorMessage(err) };
  }

  if (check.largeFileWarning) {
    const proceed = await onLargeFile({
      path: check.path,
      sizeBytes: check.fileSizeBytes,
      thresholdBytes: check.thresholdBytes,
    });
    if (!proceed) {
      return {
        kind: "cancelled-by-user",
        sizeBytes: check.fileSizeBytes,
        thresholdBytes: check.thresholdBytes,
      };
    }
  }

  try {
    const result = await aec.bim.importIfc(path);
    return { kind: "imported", result };
  } catch (err) {
    return { kind: "failed", error: errorMessage(err) };
  }
}

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return "unknown error";
  }
}
