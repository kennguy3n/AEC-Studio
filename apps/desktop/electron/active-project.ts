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
 * path; the IPC handlers update it on open/create and read it when
 * forwarding a no-projectPath call into the bridge.
 *
 * If a draft / deliver / command handler runs before a project is
 * open, `getActiveProjectPath` throws a clear validation error
 * instead of forwarding `""` into the bridge (which would either
 * fail deep in the Rust layer with an opaque IO error or — for the
 * in-process fallback — silently no-op).
 */

let activeProjectPath: string | null = null;

/**
 * Record the project path of the currently open project. Called
 * from the `project:open` and `project:createFromTemplate` IPC
 * handlers right after the bridge successfully opens / creates
 * the project package on disk.
 */
export function setActiveProjectPath(projectPath: string): void {
  if (typeof projectPath !== "string" || projectPath.length === 0) {
    throw new Error(
      "setActiveProjectPath: projectPath must be a non-empty string",
    );
  }
  activeProjectPath = projectPath;
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
 * Reset the tracker. Intended for tests; production code should
 * leave the active project set until the next `project:open`.
 */
export function clearActiveProjectPath(): void {
  activeProjectPath = null;
}
