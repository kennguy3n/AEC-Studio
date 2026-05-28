import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clearActiveProjectPath,
  getActiveProjectPath,
  peekActiveProjectPath,
  peekActiveProjectSummary,
  resolveCurrentProjectForRenderer,
  setActiveProject,
  setActiveProjectIfMatchesActive,
  setActiveProjectPath,
} from "../../../electron/active-project";

/**
 * The active-project tracker is the main-process side of the
 * renderer / bridge handshake. The renderer's `aec.draft.*` and
 * `aec.deliver.*` APIs deliberately don't carry a `projectPath` on
 * every call — pages know which project is open at most via their
 * own state, but the Rust bridge is per-project and needs the path
 * on every call.
 *
 * The IPC layer in `electron/ipc.ts` reads this tracker when
 * forwarding a no-path call into the bridge; it's set by
 * `project:open` / `project:createFromTemplate` immediately after
 * the bridge succeeds. These tests pin the contract:
 *   1. set + get round-trips a non-empty string;
 *   2. get throws a descriptive error when no project is open;
 *   3. peek returns null instead of throwing;
 *   4. clear resets the slot;
 *   5. set rejects empty strings.
 */

afterEach(() => {
  clearActiveProjectPath();
});

describe("active-project", () => {
  it("round-trips a project path", () => {
    setActiveProjectPath("/projects/demo.aecstudio");
    expect(peekActiveProjectPath()).toBe("/projects/demo.aecstudio");
    expect(getActiveProjectPath("test")).toBe("/projects/demo.aecstudio");
  });

  it("getActiveProjectPath throws with the method name when unset", () => {
    expect(() => getActiveProjectPath("deliverCreateRevision")).toThrow(
      /deliverCreateRevision: no project is currently open/,
    );
  });

  it("peekActiveProjectPath returns null when unset", () => {
    expect(peekActiveProjectPath()).toBeNull();
  });

  it("clearActiveProjectPath resets the slot", () => {
    setActiveProjectPath("/tmp/a.aecstudio");
    clearActiveProjectPath();
    expect(peekActiveProjectPath()).toBeNull();
  });

  it("setActiveProjectPath rejects empty strings", () => {
    expect(() => setActiveProjectPath("")).toThrow(/non-empty/);
  });

  it("setActiveProjectPath overwrites the previous value", () => {
    setActiveProjectPath("/projects/a.aecstudio");
    setActiveProjectPath("/projects/b.aecstudio");
    expect(peekActiveProjectPath()).toBe("/projects/b.aecstudio");
  });

  // Regression tests for `setActiveProjectIfMatchesActive` — the
  // helper that `project:save` IPC handler uses to refresh the
  // cached summary only when the saved project IS the currently
  // open one. Devin Review flagged the previous unconditional
  // `setActiveProject(summary)` as a latent contract hazard: a
  // future background-export pipeline that saves an archived
  // project would silently promote that project to the active
  // slot, racing with the renderer's `useActiveProject` hook and
  // tripping the `RequireProject` route guard mid-edit. The
  // helper encodes the invariant — "the active slot only changes
  // via project-lifecycle handlers" — at one shared location so
  // future handlers pick up the contract by importing it.
  describe("setActiveProjectIfMatchesActive", () => {
    const ACTIVE_SUMMARY = {
      projectId: "p1",
      name: "Active",
      path: "/projects/active.aecstudio",
      templateKey: "apartment",
      modifiedAt: "2026-05-27T12:00:00Z",
    };
    const REFRESHED_ACTIVE_SUMMARY = {
      ...ACTIVE_SUMMARY,
      modifiedAt: "2026-05-27T12:05:00Z",
    };
    const NON_ACTIVE_SUMMARY = {
      projectId: "p2",
      name: "Archived",
      path: "/projects/archived.aecstudio",
      templateKey: null,
      modifiedAt: "2026-05-27T12:00:00Z",
    };

    it("refreshes the cached summary when the path matches the active slot", () => {
      setActiveProject(ACTIVE_SUMMARY);
      const updated = setActiveProjectIfMatchesActive(REFRESHED_ACTIVE_SUMMARY);
      expect(updated).toBe(true);
      // The cached summary now reflects the new `modifiedAt` so a
      // subsequent `project:current` poll renders the new header
      // timestamp. The path is unchanged so the renderer hook does
      // not trip the route guard.
      expect(peekActiveProjectPath()).toBe(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()?.modifiedAt).toBe(
        "2026-05-27T12:05:00Z",
      );
    });

    it("does NOT promote a non-active project to the active slot", () => {
      setActiveProject(ACTIVE_SUMMARY);
      const updated = setActiveProjectIfMatchesActive(NON_ACTIVE_SUMMARY);
      expect(updated).toBe(false);
      // The active slot is unchanged — neither the path nor the
      // cached summary moved to the non-active project. This is the
      // load-bearing invariant: a background export pipeline that
      // saves an archived project cannot race with the renderer.
      expect(peekActiveProjectPath()).toBe(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()?.projectId).toBe("p1");
      expect(peekActiveProjectSummary()?.name).toBe("Active");
    });

    it("does NOT promote any project when no project is currently active", () => {
      // Edge case: a save runs *after* the user closes the project
      // (race window between the renderer's debounced auto-save and
      // the `project:close` IPC). The helper must not re-open the
      // closed project from a stale save callback.
      const updated = setActiveProjectIfMatchesActive(ACTIVE_SUMMARY);
      expect(updated).toBe(false);
      expect(peekActiveProjectPath()).toBeNull();
      expect(peekActiveProjectSummary()).toBeNull();
    });
  });

  // Regression: Devin Review (commit 913b768) flagged a TOCTOU in
  // the `project:current` IPC handler. The handler reads
  // `peekActiveProjectPath()` at T1, awaits `projectListRecents()`,
  // then calls `setActiveProject(found)` at T2. If
  // `clearActiveProjectPath()` runs between T1 and T2 (e.g. the
  // user closes the project during the recents lookup), the
  // handler would silently re-activate a project the user just
  // closed — leaving the renderer (which already committed the
  // close) and the main process out of sync until the next push
  // notification (which itself fires from `setActiveProject`,
  // meaning the renderer would be told to re-open the project it
  // just closed).
  //
  // The fix factors the resolution logic into
  // `resolveCurrentProjectForRenderer` and re-checks the active
  // path AFTER the await. If it diverges from the pre-await value
  // (close OR switch happened), the function returns
  // `{ summary: null }` instead of re-promoting the stale path.
  // The IPC handler is now a thin one-liner that wires the bridge
  // recents into this function, so all the logic — including the
  // TOCTOU guard — is exercised by these tests without Electron.
  describe("resolveCurrentProjectForRenderer", () => {
    const ACTIVE_SUMMARY = {
      projectId: "p1",
      name: "Active",
      path: "/projects/active.aecstudio",
      templateKey: "apartment",
      modifiedAt: "2026-05-27T12:00:00Z",
    } as const;
    const OTHER_SUMMARY = {
      projectId: "p2",
      name: "Other",
      path: "/projects/other.aecstudio",
      templateKey: null,
      modifiedAt: "2026-05-27T12:00:00Z",
    } as const;

    it("returns the cached summary directly when it is present (fast path)", async () => {
      setActiveProject(ACTIVE_SUMMARY);
      const listRecents = vi.fn().mockResolvedValue([]);

      const result = await resolveCurrentProjectForRenderer(listRecents);

      expect(result.summary?.path).toBe(ACTIVE_SUMMARY.path);
      // Fast path skips the recents lookup entirely — no bridge
      // round-trip when we already have the summary cached.
      expect(listRecents).not.toHaveBeenCalled();
    });

    it("returns null without calling listRecents when no project is open", async () => {
      const listRecents = vi.fn().mockResolvedValue([]);

      const result = await resolveCurrentProjectForRenderer(listRecents);

      expect(result.summary).toBeNull();
      expect(listRecents).not.toHaveBeenCalled();
    });

    it("resolves a path-only slot from recents and re-caches the summary", async () => {
      // The path-only branch: simulates the renderer hot-reload
      // case where the renderer reloaded but the main-process
      // tracker only has a path (no cached summary because the
      // previous `setActiveProject` happened in a prior session
      // and the main process restarted).
      setActiveProjectPath(ACTIVE_SUMMARY.path);
      // Pre-condition: the slot has a path but no summary.
      expect(peekActiveProjectSummary()).toBeNull();

      const listRecents = vi.fn().mockResolvedValue([ACTIVE_SUMMARY]);

      const result = await resolveCurrentProjectForRenderer(listRecents);

      expect(result.summary?.path).toBe(ACTIVE_SUMMARY.path);
      // Side effect: the summary is now cached so subsequent
      // calls take the fast path.
      expect(peekActiveProjectSummary()?.path).toBe(ACTIVE_SUMMARY.path);
      expect(listRecents).toHaveBeenCalledTimes(1);
    });

    it("returns null when the path is not in recents (corrupt/missing entry)", async () => {
      setActiveProjectPath(ACTIVE_SUMMARY.path);
      const listRecents = vi.fn().mockResolvedValue([OTHER_SUMMARY]);

      const result = await resolveCurrentProjectForRenderer(listRecents);

      expect(result.summary).toBeNull();
      // The slot is left in its path-only state — the renderer
      // will route to Home via the route guard, and the path is
      // cleared by the renderer's own close path if the user
      // initiates a close from Home.
      expect(peekActiveProjectPath()).toBe(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()).toBeNull();
    });

    it("does NOT re-activate a project that was closed during the recents lookup (TOCTOU guard)", async () => {
      // Set up: a path-only slot (forcing the async lookup branch).
      setActiveProjectPath(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()).toBeNull();

      // Stub recents lookup so it *also* runs the close. This
      // simulates the production race: `project:close` IPC handler
      // running during the `await getBridge().projectListRecents()`
      // suspension point. In production the racing handler is on a
      // different IPC channel; in the test, we synthesize the same
      // suspension-point race by having the listRecents callback
      // itself run the close before resolving.
      const listRecents = vi.fn().mockImplementation(async () => {
        // Yield to ensure the await actually suspends — this
        // mirrors production where the bridge's recents lookup is
        // a real cross-process call.
        await Promise.resolve();
        clearActiveProjectPath();
        return [ACTIVE_SUMMARY];
      });

      const result = await resolveCurrentProjectForRenderer(listRecents);

      // The function must bail to `summary: null` because the
      // active path changed during the await. WITHOUT the TOCTOU
      // guard, it would call `setActiveProject(ACTIVE_SUMMARY)`
      // here — silently re-activating the closed project.
      expect(result.summary).toBeNull();
      // The slot stays cleared — the close that ran during the
      // await was the user's real intent.
      expect(peekActiveProjectPath()).toBeNull();
      expect(peekActiveProjectSummary()).toBeNull();
    });

    it("does NOT overwrite the active slot when the user switched to a different project during the await", async () => {
      // The other half of the TOCTOU contract: if the user opened
      // a *different* project during the recents lookup, the
      // function must NOT clobber the new active slot with the
      // old path. Without the guard, `setActiveProject(found)`
      // would run with the pre-await path and overwrite the new
      // project's slot.
      setActiveProjectPath(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()).toBeNull();

      const listRecents = vi.fn().mockImplementation(async () => {
        await Promise.resolve();
        // User opens a different project mid-lookup. In production
        // this would be a separate `project:open` IPC handler
        // running on a separate microtask; the renderer-side
        // `useActiveProject.openProject` callback calls
        // `aec.project.open(...)` which routes through that
        // handler.
        setActiveProject(OTHER_SUMMARY);
        return [ACTIVE_SUMMARY, OTHER_SUMMARY];
      });

      const result = await resolveCurrentProjectForRenderer(listRecents);

      // The function must bail because the active path diverged.
      // The renderer will re-poll on its next tick (or be told via
      // push notification) and the fast path returns OTHER_SUMMARY.
      expect(result.summary).toBeNull();
      // The new project remains active — the function did not
      // clobber it with the old path's summary.
      expect(peekActiveProjectPath()).toBe(OTHER_SUMMARY.path);
      expect(peekActiveProjectSummary()?.path).toBe(OTHER_SUMMARY.path);
    });

    it("succeeds when the active path is unchanged across the await (happy path)", async () => {
      // Sanity: when no race happens, the function still completes
      // successfully and re-caches the summary. This guards
      // against a regression where an over-eager TOCTOU guard
      // could break the legitimate path-only branch.
      setActiveProjectPath(ACTIVE_SUMMARY.path);
      const listRecents = vi.fn().mockImplementation(async () => {
        // Yield to suspend; do NOT touch the active slot.
        await Promise.resolve();
        return [ACTIVE_SUMMARY];
      });

      const result = await resolveCurrentProjectForRenderer(listRecents);

      expect(result.summary?.path).toBe(ACTIVE_SUMMARY.path);
      expect(peekActiveProjectSummary()?.path).toBe(ACTIVE_SUMMARY.path);
    });
  });
});
