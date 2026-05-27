/**
 * Active-project tracker for the Electron main process.
 *
 * The renderer's public `window.aec.*` API doesn't always expose a
 * project path on every call — pages call `aec.deliver.listRevisions()`
 * or `aec.draft.importDxf({ dxfPath })` without threading the currently
 * open project's filesystem path. The Rust bridge service, however,
 * is per-project: every `draft.*` / `deliver.*` / `command.*` method
 * needs to know which encrypted SQLite database to open.
 *
 * The main process is the natural owner of "which project is open
 * right now" — it's the side that runs `project:open` and
 * `project:createFromTemplate` and holds the on-disk handle. This
 * module keeps a single process-wide slot for the active project
 * (path + cached `ProjectSummary`); the IPC handlers update it on
 * open/create/save/close and read it when forwarding a no-projectPath
 * call into the bridge or answering a `project:current` poll from
 * the renderer's `useActiveProject` hook.
 *
 * If a draft / deliver / command handler runs before a project is
 * open, `getActiveProjectPath` throws a clear validation error
 * instead of forwarding `""` into the bridge (which would either
 * fail deep in the Rust layer with an opaque IO error or — for the
 * in-process fallback — silently no-op).
 */

/**
 * Project summary shape mirrored from `crates/aec_core::package::ProjectSummary`.
 * Inlined here rather than imported from `bridge.ts` so the
 * tracker has no compile-time dependency on the bridge module
 * (which pulls in the napi binary). The bridge `ProjectSummary`
 * type is structurally compatible — the tracker accepts any
 * object with these fields, and `setActiveProject` runs a
 * structural check.
 */
export interface ActiveProjectSummary {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
}

let activeProjectPath: string | null = null;
let activeProjectSummary: ActiveProjectSummary | null = null;

type ChangeListener = (
  summary: ActiveProjectSummary | null,
) => void;

const listeners = new Set<ChangeListener>();

/**
 * Record the project path of the currently open project. Called
 * from the `project:open` and `project:createFromTemplate` IPC
 * handlers right after the bridge successfully opens / creates
 * the project package on disk.
 *
 * Prefer [`setActiveProject`] when a full `ProjectSummary` is
 * available — it caches the summary so the renderer's
 * `useActiveProject` hook can resolve `name` / `templateKey`
 * without a follow-up bridge round-trip.
 */
export function setActiveProjectPath(projectPath: string): void {
  if (typeof projectPath !== "string" || projectPath.length === 0) {
    throw new Error(
      "setActiveProjectPath: projectPath must be a non-empty string",
    );
  }
  activeProjectPath = projectPath;
  // If the cached summary doesn't match the new path, clear it so a
  // follow-up `project:current` returns `null`-summary with the
  // path-only fallback rather than the previous project's summary.
  if (
    activeProjectSummary !== null &&
    activeProjectSummary.path !== projectPath
  ) {
    activeProjectSummary = null;
  }
  notify();
}

/**
 * Record the full summary of the currently open project. Used by
 * `project:open`, `project:createFromTemplate`, and `project:save`
 * so the renderer's hook can render the project name in the app
 * header without a bridge round-trip. Throws if `summary.path` is
 * empty (mirrors `setActiveProjectPath`).
 */
export function setActiveProject(summary: ActiveProjectSummary): void {
  if (typeof summary.path !== "string" || summary.path.length === 0) {
    throw new Error("setActiveProject: summary.path must be a non-empty string");
  }
  activeProjectPath = summary.path;
  activeProjectSummary = { ...summary };
  notify();
}

/**
 * Look up the project path of the currently open project. Throws
 * if no project is open — handlers that depend on an active
 * project must surface this as a clean validation error rather
 * than passing `""` into the bridge.
 */
export function getActiveProjectPath(method: string): string {
  if (activeProjectPath === null) {
    throw new Error(
      `${method}: no project is currently open. Call \`project.open\` ` +
        `or \`project.createFromTemplate\` first.`,
    );
  }
  return activeProjectPath;
}

/**
 * Look up the project path, returning `null` if no project is
 * open. Handlers that *prefer* an explicit caller-supplied
 * `projectPath` over the tracked active project (e.g. when a
 * renderer call carries it inline) use this to opt into the
 * fallback without throwing.
 */
export function peekActiveProjectPath(): string | null {
  return activeProjectPath;
}

/**
 * Refresh the cached summary *only* when the given summary belongs
 * to the currently-open project. Returns `true` when the cache was
 * updated, `false` otherwise.
 *
 * Used by the `project:save` IPC handler so that saving the active
 * project picks up the new `modifiedAt` for the header, while a
 * non-active caller (a future background export pipeline that saves
 * an archived project, a multi-project bulk-save batch job) cannot
 * silently promote a different project into the active slot — which
 * would race with the renderer's `useActiveProject` hook and trip
 * the `RequireProject` route guard mid-edit. The active slot only
 * ever changes via `project:open` / `project:createFromTemplate` /
 * `project:close`, or a save of the project that is already active.
 *
 * The cross-handler invariant ("only project-lifecycle IPC handlers
 * move the active slot") is enforced *here* rather than inlined at
 * each call site so future handlers that need the same behaviour
 * (e.g. `project:exportPackage` if it grows a save-side-effect)
 * pick up the same contract by importing this helper.
 */
export function setActiveProjectIfMatchesActive(
  summary: ActiveProjectSummary,
): boolean {
  if (activeProjectPath !== summary.path) {
    return false;
  }
  setActiveProject(summary);
  return true;
}

/**
 * Look up the cached summary of the currently open project. Returns
 * `null` if no project is open or no summary has been cached (only
 * `setActiveProjectPath` was called — in that case the renderer
 * should fall back to `aec.project.listRecents()` to look up the
 * full summary).
 */
export function peekActiveProjectSummary(): ActiveProjectSummary | null {
  return activeProjectSummary === null ? null : { ...activeProjectSummary };
}

/**
 * Reset the tracker. Three callers:
 *
 *   1. Unit tests (`vitest`) that need a clean slot between cases
 *      — the test file uses `afterEach(clearActiveProjectPath)`.
 *   2. The `project:close` IPC handler (Phase 13 Task 22).
 *   3. Window-close cleanup on the main process side, when added.
 *
 * Earlier the only way the user could transition out of a project
 * was to open another one (`project:open` /
 * `project:createFromTemplate`), and both of those overwrite the
 * slot via `setActiveProjectPath` after the bridge has successfully
 * opened the new project package. Phase 13 adds the
 * `project:close` handler so the renderer can return the user to
 * the Home screen without a hard reload.
 */
export function clearActiveProjectPath(): void {
  activeProjectPath = null;
  activeProjectSummary = null;
  notify();
}

/**
 * Subscribe to active-project changes. The listener is called
 * synchronously from `setActive*` / `clear*` calls so the renderer's
 * `project:current` IPC channel can stream live updates if a
 * future feature wants push-based notifications. Returns an
 * unsubscribe function.
 */
export function onActiveProjectChange(listener: ChangeListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function notify(): void {
  const snapshot = activeProjectSummary === null ? null : { ...activeProjectSummary };
  for (const l of Array.from(listeners)) {
    try {
      l(snapshot);
    } catch {
      // Defensive: a listener throwing must not corrupt the tracker
      // state or block other listeners. The Electron main process
      // has no global error handler that would catch a sync throw
      // out of an `ipcMain.handle` callback, so swallow here.
    }
  }
}
