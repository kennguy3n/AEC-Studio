import { afterEach, describe, expect, it } from "vitest";
import {
  clearActiveProjectPath,
  getActiveProjectPath,
  peekActiveProjectPath,
  peekActiveProjectSummary,
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
});
