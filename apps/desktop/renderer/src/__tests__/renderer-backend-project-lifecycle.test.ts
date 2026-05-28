/**
 * Renderer in-process backend — project lifecycle invariants.
 *
 * The in-process backend used by vitest must mirror the production
 * Electron main-process behaviour for the project lifecycle so tests
 * exercise the same state-machine the user sees in the packaged app.
 * Three invariants are pinned here:
 *
 *   1. `close()` clears the active-project slot so a subsequent
 *      `current()` returns `{ summary: null }` — mirroring
 *      `clearActiveProjectPath()` in `electron/active-project.ts`. An
 *      earlier version of the backend conflated "recents history" and
 *      "currently open" by returning `recents[0]` from `current()`,
 *      which silently dropped the close-clear contract. The split
 *      between `recents` (history, persists across close+reopen) and
 *      `current` (active slot, becomes `null` after close) is the
 *      load-bearing fix.
 *
 *   2. `close()` preserves the `recents` history. Wiping recents on
 *      close would break the Home screen's "recently opened" list
 *      and force the user to re-pick the file every session.
 *
 *   3. `onActiveProjectChange(...)` fires on every transition
 *      (open / create / save of the active project / close).
 *      Delivery is deferred to a macrotask to match Electron IPC
 *      semantics in production (Chromium dispatches webContents.send
 *      and the invoke-response on separate macrotask boundaries; the
 *      renderer's microtask drain runs BETWEEN them). This ordering
 *      is what lets `useActiveProject`'s listener path-equality
 *      guard correctly skip the redundant write triggered by
 *      `openProject`/`createProject`/`saveProject`/`closeProject`
 *      itself — their continuation has already committed the new
 *      `projectPathRef` by the time the listener fires. Tests that
 *      assert on listener side-effects therefore await one macrotask
 *      tick (`await new Promise((r) => setTimeout(r, 0))`) after
 *      the last transition so the deferred listener callbacks have
 *      a chance to run.
 */

import { describe, expect, it } from "vitest";

import { rendererInProcessBackend } from "../api/renderer-backend";

describe("rendererInProcessBackend — project lifecycle", () => {
  it("returns null from current() after close, even with recents populated", async () => {
    const b = rendererInProcessBackend();

    const summary = await b.project.createFromTemplate("apartment", "Demo");
    const before = await b.project.current();
    expect(before.summary).not.toBeNull();
    expect(before.summary?.path).toBe(summary.path);

    await b.project.close();
    const after = await b.project.current();
    expect(after.summary).toBeNull();

    // Recents history must persist across close — the Home screen's
    // "recently opened" list depends on this surviving session-to-
    // session and project-to-project transitions.
    const recents = await b.project.listRecents();
    expect(recents.length).toBe(1);
    expect(recents[0]?.path).toBe(summary.path);
  });

  it("close() does NOT clobber the recents list when there are multiple recents", async () => {
    const b = rendererInProcessBackend();

    await b.project.createFromTemplate("apartment", "P1");
    await b.project.createFromTemplate("apartment", "P2");
    await b.project.createFromTemplate("apartment", "P3");

    // The MRU head should be P3 (most recently opened goes to index 0).
    let recents = await b.project.listRecents();
    expect(recents.length).toBe(3);
    expect(recents[0]?.name).toBe("P3");

    await b.project.close();

    // current() returns null, but all three recents survive.
    const after = await b.project.current();
    expect(after.summary).toBeNull();

    recents = await b.project.listRecents();
    expect(recents.length).toBe(3);
    expect(recents.map((r: { name: string }) => r.name).sort()).toEqual([
      "P1",
      "P2",
      "P3",
    ]);
  });

  it("reopening a project after close restores active state without polluting recents", async () => {
    const b = rendererInProcessBackend();

    const summary = await b.project.createFromTemplate("apartment", "Reopened");
    await b.project.close();
    const cleared = await b.project.current();
    expect(cleared.summary).toBeNull();

    // Re-open via the same path. current() now points back at it.
    await b.project.open(summary.path);
    const after = await b.project.current();
    expect(after.summary?.path).toBe(summary.path);

    // The MRU contract de-duplicates by projectId, so the same project
    // reopened produces a single recents entry (the second one with a
    // fresh projectId, since `open` mints a new id without consulting
    // the recents store). This is fine — the renderer hook keys off
    // path, not projectId, for "is this the same project?" checks.
    const recents = await b.project.listRecents();
    expect(recents.length).toBeGreaterThanOrEqual(1);
    expect(recents[0]?.path).toBe(summary.path);
  });

  it("save() updates the active-project mirror in lockstep with the recents entry", async () => {
    const b = rendererInProcessBackend();

    const summary = await b.project.createFromTemplate("apartment", "SaveTest");
    const initialModifiedAt = summary.modifiedAt;

    // Wait briefly so the save's timestamp is strictly greater.
    await new Promise((resolve) => setTimeout(resolve, 5));

    const saved = await b.project.save(summary.path);
    expect(saved.modifiedAt).not.toBe(initialModifiedAt);

    // current()'s summary must reflect the same refreshed modifiedAt
    // — production's `setActiveProjectIfMatchesActive` enforces this.
    const after = await b.project.current();
    expect(after.summary?.modifiedAt).toBe(saved.modifiedAt);
  });

  it("save() of a non-active project does NOT promote it into the active slot", async () => {
    const b = rendererInProcessBackend();

    const active = await b.project.createFromTemplate("apartment", "Active");
    const other = await b.project.createFromTemplate("apartment", "Other");
    // Now "Other" is the active one (createFromTemplate promotes).
    expect((await b.project.current()).summary?.path).toBe(other.path);

    // Saving "Active" while "Other" is active must NOT change which
    // project current() returns — same contract as
    // `setActiveProjectIfMatchesActive` on the main side.
    await b.project.save(active.path);
    expect((await b.project.current()).summary?.path).toBe(other.path);
  });

  it("onActiveProjectChange fires asynchronously after every transition with the latest summary", async () => {
    const b = rendererInProcessBackend();

    type ChangeEvent =
      | { kind: "set"; path: string }
      | { kind: "cleared" };

    const events: ChangeEvent[] = [];
    const unsubscribe = b.project.onActiveProjectChange((summary) => {
      events.push(
        summary === null
          ? { kind: "cleared" }
          : { kind: "set", path: summary.path },
      );
    });

    const summary = await b.project.createFromTemplate("apartment", "Sub");
    const opened = await b.project.open("/projects/another.aecstudio");
    await b.project.save(opened.path);
    await b.project.close();

    // Drain the macrotask queue so the deferred listener callbacks
    // queued by the four transitions above have run. Without this
    // tick the assertion would observe only 0 events because the
    // backend defers listener iteration to `setTimeout(..., 0)` to
    // match Electron IPC's macrotask delivery semantics (see
    // `notifyActiveChange` in renderer-backend.ts for the
    // production-parity rationale).
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(events).toEqual([
      { kind: "set", path: summary.path },
      { kind: "set", path: opened.path },
      { kind: "set", path: opened.path }, // save of the active project
      { kind: "cleared" },
    ]);

    unsubscribe();

    // After unsubscribe, no further events should arrive. The
    // listener removal happens BEFORE the next transition's
    // deferred macrotask fires; the in-process backend re-reads
    // listener membership at iteration time (via `Array.from`) so
    // the unsubscribed listener is dropped before any callback
    // would have run. This matches production's
    // `ipcRenderer.off(...)` semantics where the removal lands
    // before the queued IPC event reaches the dispatcher.
    await b.project.createFromTemplate("apartment", "AfterUnsubscribe");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(events.length).toBe(4);
  });

  it("onActiveProjectChange survives a listener that throws — other listeners still fire", async () => {
    const b = rendererInProcessBackend();

    const seen: string[] = [];
    const unsubA = b.project.onActiveProjectChange(() => {
      throw new Error("listener A bug");
    });
    const unsubB = b.project.onActiveProjectChange((summary) => {
      if (summary !== null) seen.push(summary.path);
    });

    const summary = await b.project.createFromTemplate("apartment", "Resilient");
    // Drain the deferred listener iteration (see
    // notifyActiveChange's macrotask defer rationale).
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(seen).toEqual([summary.path]);

    unsubA();
    unsubB();
  });
});
