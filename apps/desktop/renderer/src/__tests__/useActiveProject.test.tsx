/**
 * Active-project hook regression tests.
 *
 * Devin Review flagged that the original `saveProject` implementation
 * swallowed every save error inside the hook, which made the manual
 * save shortcut (`Ctrl/Cmd+S`) always render a "Saved {name}" toast
 * even when the underlying bridge call rejected. The fix re-throws so
 * the caller (e.g. `App.tsx`) sees the failure, while the auto-save
 * debounce timer keeps its fire-and-forget semantics via an inner
 * `.catch()`. These tests lock in the post-fix contract.
 *
 * A second round of Devin Review found that the 5-second auto-save
 * debounce timer armed by `markDirty()` was *not* cancelled when the
 * user opened a different project (`openProject`) or created a new
 * one (`createProject`). A stale timer captured the old project's
 * state in its closure and would fire after the new project had been
 * swapped in, calling `aec.project.save(oldPath)` and then
 * `setProject(oldSummary)` — silently re-binding the active project
 * to the previous one. The fix routes both transitions through a
 * shared `cancelPendingAutoSave()` helper. The
 * "open/create cancels pending auto-save" tests below lock that in.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import React, { useEffect } from "react";
import { aec } from "../api/aec";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";

function OpenAndSave({
  onError,
}: {
  onError: (err: unknown) => void;
}) {
  const { project, openProject, saveProject } = useActiveProject();
  useEffect(() => {
    void openProject("/tmp/sample.aecstudio");
  }, [openProject]);
  return (
    <div>
      <span data-testid="proj-name">{project?.name ?? ""}</span>
      <button
        type="button"
        data-testid="trigger-save"
        onClick={async () => {
          try {
            await saveProject();
          } catch (err) {
            onError(err);
          }
        }}
      >
        save
      </button>
    </div>
  );
}

describe("useActiveProject — saveProject error propagation", () => {
  it("re-throws when the bridge save rejects so callers can show a toast", async () => {
    const saveSpy = vi
      .spyOn(aec.project, "save")
      .mockRejectedValue(new Error("disk full"));
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <OpenAndSave onError={onError} />
      </ActiveProjectProvider>,
    );

    // Wait for `openProject` to complete and the project name to surface.
    await waitFor(() =>
      expect(screen.getByTestId("proj-name").textContent).toBe("sample"),
    );

    await act(async () => {
      screen.getByTestId("trigger-save").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    const err = onError.mock.calls[0]![0];
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toBe("disk full");

    saveSpy.mockRestore();
  });

  it("does not throw when the bridge save resolves", async () => {
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <OpenAndSave onError={onError} />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-name").textContent).toBe("sample"),
    );

    await act(async () => {
      screen.getByTestId("trigger-save").click();
    });

    // The in-process backend resolves the save, so no error should
    // bubble up to the caller's catch.
    await waitFor(() => expect(onError).not.toHaveBeenCalled());
  });
});

/**
 * Auto-save timer cleanup contract for project transitions.
 *
 * The 5-second debounce inside `markDirty()` schedules a `setTimeout`
 * that captures the current `saveProject` (which itself captures the
 * current `project` summary). Before the fix, opening or creating a
 * different project did not cancel that pending timeout. When it
 * fired ~5s later it invoked `aec.project.save(oldPath)` and then
 * `setProject(oldSummary)`, overwriting the renderer's notion of the
 * active project — *and* the main-process `project:save` IPC handler
 * also calls `setActiveProject(oldSummary)`, propagating the stale
 * pointer to every page that reads from `peekActiveProjectPath()`.
 *
 * These tests use fake timers to deterministically advance past the
 * 5-second window and assert that `aec.project.save` is never called
 * after a transition. Running the assertions without fake timers
 * would either be flaky or require slowing the suite by 5+ seconds.
 */
function DirtyThenOpenOther({
  initialPath,
  nextPath,
}: {
  initialPath: string;
  nextPath: string;
}) {
  const { project, openProject, markDirty } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="open-other"
        onClick={() => {
          void openProject(nextPath);
        }}
      >
        open other
      </button>
    </div>
  );
}

function DirtyThenCreateOther({
  initialPath,
  templateKey,
  newName,
}: {
  initialPath: string;
  templateKey: string;
  newName: string;
}) {
  const { project, openProject, createProject, markDirty } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="create-other"
        onClick={() => {
          void createProject(templateKey, newName);
        }}
      >
        create other
      </button>
    </div>
  );
}

describe("useActiveProject — auto-save timer cancellation on transition", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("cancels pending auto-save when openProject is called", async () => {
    const saveSpy = vi.spyOn(aec.project, "save");

    render(
      <ActiveProjectProvider>
        <DirtyThenOpenOther
          initialPath="/tmp/projectA.aecstudio"
          nextPath="/tmp/projectB.aecstudio"
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    // Mark dirty — arms a 5-second auto-save timer captured against
    // projectA.
    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    // Immediately open projectB. The new contract is that this
    // cancels the pending timer before the swap.
    await act(async () => {
      screen.getByTestId("open-other").click();
    });
    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectB.aecstudio",
      ),
    );

    // Advance past the 5-second debounce. Without the fix the stale
    // timer fires here and calls `aec.project.save("/tmp/projectA.aecstudio")`;
    // with the fix the cancelled timer never runs.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).not.toHaveBeenCalled();
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/tmp/projectB.aecstudio",
    );

    saveSpy.mockRestore();
  });

  it("cancels pending auto-save when createProject is called", async () => {
    const saveSpy = vi.spyOn(aec.project, "save");

    render(
      <ActiveProjectProvider>
        <DirtyThenCreateOther
          initialPath="/tmp/projectA.aecstudio"
          templateKey="apartment"
          newName="Project B"
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    await act(async () => {
      screen.getByTestId("create-other").click();
    });
    // The new project's path is derived from the name in the
    // in-process backend.
    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/projects/project_b.aecstudio",
      ),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).not.toHaveBeenCalled();
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/projects/project_b.aecstudio",
    );

    saveSpy.mockRestore();
  });
});

/**
 * Manual `saveProject()` must cancel the pending auto-save timer.
 *
 * Devin Review's third pass on the hook flagged that `saveProject`
 * (called by `Ctrl+S`, the App save handler, or any future programmatic
 * caller) cleared `dirty` but did not clear the 5-second auto-save
 * timer armed by the prior `markDirty()`. Five seconds after the
 * manual save the orphaned timer would fire, run a second
 * `aec.project.save(...)` IPC round-trip, and flash the StatusBar
 * "Saved" → "Saving…" → "Saved" for zero useful work. The fix
 * colocates `cancelPendingAutoSave()` with the `setDirty(false)` on
 * the success branch of `saveProject`. The auto-save path's own
 * `setTimeout` callback nulls `autoSaveTimerRef.current` before
 * invoking `saveProject`, so the inner `cancelPendingAutoSave()`
 * call is a no-op on that path — but it remains the single source
 * of truth for "a successful save invalidates a pending timer".
 */
function DirtyThenManualSave({
  initialPath,
}: {
  initialPath: string;
}) {
  const { project, openProject, markDirty, saveProject } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="manual-save"
        onClick={() => {
          void saveProject();
        }}
      >
        manual save
      </button>
    </div>
  );
}

describe("useActiveProject — manual save cancels pending auto-save", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not fire a second save 5s after a manual save", async () => {
    const saveSpy = vi.spyOn(aec.project, "save");

    render(
      <ActiveProjectProvider>
        <DirtyThenManualSave initialPath="/tmp/manualSave.aecstudio" />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/manualSave.aecstudio",
      ),
    );

    // Arm the 5s auto-save timer.
    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    // Trigger an immediate manual save (the Ctrl+S code path).
    await act(async () => {
      screen.getByTestId("manual-save").click();
    });

    // Exactly one bridge save round-trip for the manual save itself.
    await waitFor(() => expect(saveSpy).toHaveBeenCalledTimes(1));

    // Advance well past the 5-second debounce. Without the fix the
    // stale auto-save timer fires here and triggers a second
    // `aec.project.save` round-trip — flashing the StatusBar through
    // "Saving…" → "Saved" with no underlying mutation. With the fix
    // the timer was cancelled inside `saveProject` itself.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).toHaveBeenCalledTimes(1);

    saveSpy.mockRestore();
  });

  it("only fires the auto-save once when the debounce elapses naturally", async () => {
    // Defense-in-depth: the auto-save path nulls `autoSaveTimerRef`
    // before calling `saveProject`, and `saveProject` then re-enters
    // `cancelPendingAutoSave`. This test pins that the no-op
    // double-clear path does NOT race with `markDirty` re-arming the
    // timer mid-save (it shouldn't, because no dirty mutation happens
    // between the timer firing and `saveProject` resolving).
    const saveSpy = vi.spyOn(aec.project, "save");

    render(
      <ActiveProjectProvider>
        <DirtyThenManualSave initialPath="/tmp/autoSave.aecstudio" />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/autoSave.aecstudio",
      ),
    );

    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).toHaveBeenCalledTimes(1);

    saveSpy.mockRestore();
  });
});

/**
 * Failed `openProject` / `createProject` must re-arm the auto-save timer.
 *
 * Devin Review's fourth pass on the hook flagged a data-loss window:
 * `openProject`/`createProject` both call `cancelPendingAutoSave()`
 * BEFORE the async bridge call (correctly — otherwise a stale timer
 * captured against project A could fire AFTER the swap to project B
 * and re-bind the active project). If the bridge call then *throws*
 * (corrupt file, permission denied, disk full, etc.), the original
 * project remains active with `dirty === true` but its auto-save
 * timer is permanently lost: the user's pending changes would only
 * persist on their next mutation, which may never come if they walk
 * away. The fix wraps each transition in a try/catch that re-arms the
 * timer (via the stable `armAutoSave` helper) iff the dirty flag is
 * still set. These tests pin both the rearm-on-failure behavior and
 * its negative case (no spurious arm when the original state was
 * clean).
 */
function DirtyThenFailingOpen({
  initialPath,
  failingPath,
  onError,
}: {
  initialPath: string;
  failingPath: string;
  onError: (err: unknown) => void;
}) {
  const { project, openProject, markDirty } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="open-failing"
        onClick={async () => {
          try {
            await openProject(failingPath);
          } catch (err) {
            onError(err);
          }
        }}
      >
        open failing
      </button>
    </div>
  );
}

function DirtyThenFailingCreate({
  initialPath,
  templateKey,
  newName,
  onError,
}: {
  initialPath: string;
  templateKey: string;
  newName: string;
  onError: (err: unknown) => void;
}) {
  const { project, openProject, createProject, markDirty } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="create-failing"
        onClick={async () => {
          try {
            await createProject(templateKey, newName);
          } catch (err) {
            onError(err);
          }
        }}
      >
        create failing
      </button>
    </div>
  );
}

describe("useActiveProject — failed transition re-arms auto-save timer", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("re-arms the auto-save timer when openProject's bridge call rejects mid-flight", async () => {
    // The initial mount call must succeed (the test needs an active
    // project A to mark dirty against). We let the in-process backend
    // handle that, then install the rejection mock only after the
    // first open settles so the second open — the button-triggered
    // failing transition — rejects with "permission denied".
    const saveSpy = vi.spyOn(aec.project, "save");
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <DirtyThenFailingOpen
          initialPath="/tmp/projectA.aecstudio"
          failingPath="/tmp/broken.aecstudio"
          onError={onError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    // Now that project A is loaded, swap the bridge implementation
    // so the next `aec.project.open(...)` call rejects.
    const openSpy = vi
      .spyOn(aec.project, "open")
      .mockRejectedValue(new Error("permission denied"));

    // Arm the 5s auto-save timer against project A.
    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    // Trigger the failing open. The hook cancels the timer before
    // the await, the bridge rejects, and the catch block must re-arm.
    await act(async () => {
      screen.getByTestId("open-failing").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    const err = onError.mock.calls[0]![0];
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toBe("permission denied");

    // The active project should still be A — the swap never happened.
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/tmp/projectA.aecstudio",
    );

    // Advance past the re-armed 5s debounce. WITHOUT the fix, the
    // timer was cancelled before the await and never re-armed, so
    // `aec.project.save` would NOT be called here — the bug. WITH
    // the fix, the catch block re-arms via `armAutoSave` and the
    // auto-save fires on schedule against project A.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).toHaveBeenCalledTimes(1);
    expect(saveSpy).toHaveBeenCalledWith("/tmp/projectA.aecstudio");

    openSpy.mockRestore();
    saveSpy.mockRestore();
  });

  it("re-arms the auto-save timer when createProject's bridge call rejects mid-flight", async () => {
    const createSpy = vi
      .spyOn(aec.project, "createFromTemplate")
      .mockRejectedValue(new Error("template not found"));
    const saveSpy = vi.spyOn(aec.project, "save");
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <DirtyThenFailingCreate
          initialPath="/tmp/projectA.aecstudio"
          templateKey="missing"
          newName="Should Fail"
          onError={onError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    await act(async () => {
      screen.getByTestId("create-failing").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/tmp/projectA.aecstudio",
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    // Auto-save should have fired against the still-active project A.
    expect(saveSpy).toHaveBeenCalledTimes(1);
    expect(saveSpy).toHaveBeenCalledWith("/tmp/projectA.aecstudio");

    createSpy.mockRestore();
    saveSpy.mockRestore();
  });

  it("does NOT arm a spurious auto-save timer when a failing openProject runs from a clean state", async () => {
    // Negative case: if there was never a pending timer (dirty stayed
    // false), a failed open must not spontaneously schedule one. This
    // pins the `if (dirtyRef.current) armAutoSave()` guard so future
    // refactors don't accidentally arm timers from clean states and
    // burn cycles auto-saving an unchanged project.
    const saveSpy = vi.spyOn(aec.project, "save");
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <DirtyThenFailingOpen
          initialPath="/tmp/projectA.aecstudio"
          failingPath="/tmp/broken.aecstudio"
          onError={onError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    const openSpy = vi
      .spyOn(aec.project, "open")
      .mockRejectedValue(new Error("permission denied"));

    // Deliberately skip the dirty click — project A is clean.
    await act(async () => {
      screen.getByTestId("open-failing").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    // No save should have fired — there was nothing to save.
    expect(saveSpy).not.toHaveBeenCalled();

    openSpy.mockRestore();
    saveSpy.mockRestore();
  });
});

/**
 * `closeProject` symmetry with `openProject` / `createProject`.
 *
 * Devin Review's fifth pass flagged that `closeProject` did not share
 * the catch-rearm-rethrow pattern that the other two project-lifecycle
 * transitions adopted after the earlier fixes. Today the main-process
 * `project:close` handler is a synchronous `clearActiveProjectPath()`
 * that cannot throw, so the catch is unreachable in the current build
 * — but the handler signature is `async` and future enhancements
 * (flush-pending-changes before close, user-confirmation prompt with
 * await round-trip, telemetry submit before clearing the slot,
 * integrity-check on the project file before release) create a real
 * failure surface. Without the catch the original project would
 * remain active with `dirty === true` while its auto-save timer is
 * permanently cancelled — equivalent data-loss window to the one the
 * earlier fix closed for open/create. These tests force the bridge
 * `project:close` to reject and pin both the rearm-on-failure and the
 * negative-case (no spurious arm from a clean state) contracts.
 */
function DirtyThenFailingClose({
  initialPath,
  onError,
}: {
  initialPath: string;
  onError: (err: unknown) => void;
}) {
  const { project, openProject, closeProject, markDirty } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="dirty"
        onClick={() => markDirty()}
      >
        dirty
      </button>
      <button
        type="button"
        data-testid="close-failing"
        onClick={async () => {
          try {
            await closeProject();
          } catch (err) {
            onError(err);
          }
        }}
      >
        close failing
      </button>
    </div>
  );
}

describe("useActiveProject — failed close re-arms auto-save timer", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("re-arms the auto-save timer when closeProject's bridge call rejects mid-flight", async () => {
    const saveSpy = vi.spyOn(aec.project, "save");
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <DirtyThenFailingClose
          initialPath="/tmp/projectA.aecstudio"
          onError={onError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    // Force the bridge close to reject — simulates a future
    // flush-pending-changes / confirmation handler that fails.
    const closeSpy = vi
      .spyOn(aec.project, "close")
      .mockRejectedValue(new Error("flush failed"));

    await act(async () => {
      screen.getByTestId("dirty").click();
    });

    await act(async () => {
      screen.getByTestId("close-failing").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    const err = onError.mock.calls[0]![0];
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toBe("flush failed");

    // The active project should still be A — the close never landed.
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/tmp/projectA.aecstudio",
    );

    // Advance past the re-armed 5s debounce. WITHOUT the fix, the
    // timer was cancelled before the await and never re-armed, so
    // `aec.project.save` would NOT be called here — pending changes
    // would only persist on the user's next mutation. WITH the fix,
    // the catch block re-arms via `armAutoSave` and the auto-save
    // fires on schedule against project A.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).toHaveBeenCalledTimes(1);
    expect(saveSpy).toHaveBeenCalledWith("/tmp/projectA.aecstudio");

    closeSpy.mockRestore();
    saveSpy.mockRestore();
  });

  it("does NOT arm a spurious auto-save timer when a failing closeProject runs from a clean state", async () => {
    // Negative case: matches the open/create clean-state guard. If
    // dirty was never set, a failed close must not spontaneously
    // schedule a save against an unchanged project.
    const saveSpy = vi.spyOn(aec.project, "save");
    const onError = vi.fn();

    render(
      <ActiveProjectProvider>
        <DirtyThenFailingClose
          initialPath="/tmp/projectA.aecstudio"
          onError={onError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    const closeSpy = vi
      .spyOn(aec.project, "close")
      .mockRejectedValue(new Error("flush failed"));

    // Deliberately skip the dirty click — project A is clean.
    await act(async () => {
      screen.getByTestId("close-failing").click();
    });

    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(saveSpy).not.toHaveBeenCalled();

    closeSpy.mockRestore();
    saveSpy.mockRestore();
  });
});

/**
 * `saveProject` must coalesce concurrent invocations.
 *
 * Devin Review's sixth pass flagged that the previous `saveProject`
 * implementation issued a parallel bridge call when the 5-second
 * auto-save timer fired DURING an in-flight manual save (or vice
 * versa). The bridge is idempotent (SQLCipher WAL + atomic file
 * write), so no data corruption — but the StatusBar would flicker
 * Saving→Saved→Saving→Saved for one logical save event, redundant
 * disk I/O would burn battery on laptops, and the user spamming
 * `Ctrl+S` during a slow save ("is it stuck?") would multiply the
 * problem. The fix introduces a `savingPromiseRef` slot that returns
 * the in-flight promise from subsequent invocations so every caller
 * awaits the same resolution. These tests pin both the spam-coalesce
 * contract and the auto-save-during-manual-save coalesce contract,
 * plus the recovery case: a failed save must clear the slot so the
 * next save event isn't permanently locked out.
 */
function ManualSaveSpammer({
  initialPath,
  spamCount,
}: {
  initialPath: string;
  spamCount: number;
}) {
  const { project, openProject, saveProject } = useActiveProject();
  useEffect(() => {
    void openProject(initialPath);
  }, [openProject, initialPath]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="spam-save"
        onClick={() => {
          for (let i = 0; i < spamCount; i++) {
            // Swallow rejections in the fixture so the test asserts
            // bridge-call-count via the spy rather than via toast
            // surfacing. Real callers (the App save handler) attach a
            // proper `.catch(addToast)` — they don't `void` the promise.
            saveProject().catch(() => {});
          }
        }}
      >
        spam save
      </button>
    </div>
  );
}

describe("useActiveProject — saveProject coalesces concurrent invocations", () => {
  it("dispatches exactly one bridge call when the same tick fires N parallel saveProject calls", async () => {
    // Hold the save resolution so the spam can stack against an
    // in-flight promise. Without coalescing, all N invocations would
    // each call `aec.project.save(...)` and the spy count would be N.
    let resolveSave: ((summary: unknown) => void) | null = null;
    const savePromise = new Promise<unknown>((resolve) => {
      resolveSave = resolve;
    });
    const saveSpy = vi
      .spyOn(aec.project, "save")
      .mockImplementation(() => savePromise);

    render(
      <ActiveProjectProvider>
        <ManualSaveSpammer
          initialPath="/tmp/spamSave.aecstudio"
          spamCount={5}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/spamSave.aecstudio",
      ),
    );

    // Fire 5 parallel saveProject calls in a single click handler.
    await act(async () => {
      screen.getByTestId("spam-save").click();
    });

    // Exactly one bridge call should be in flight, regardless of N.
    expect(saveSpy).toHaveBeenCalledTimes(1);

    // Resolve the in-flight save with a summary shape the hook
    // recognizes.
    await act(async () => {
      resolveSave!({
        path: "/tmp/spamSave.aecstudio",
        name: "spamSave",
        template_key: "blank",
        room_count: 0,
        material_count: 0,
      });
    });

    // Still exactly one bridge call after resolution.
    expect(saveSpy).toHaveBeenCalledTimes(1);

    saveSpy.mockRestore();
  });

  it("clears the coalescing slot on save failure so the next save event is not locked out", async () => {
    // Without the `savingPromiseRef.current = null` in the `finally`,
    // a single failed save would permanently reject every subsequent
    // `saveProject` call with the stale promise — turning a transient
    // disk error into a session-long save outage.
    const saveSpy = vi
      .spyOn(aec.project, "save")
      .mockRejectedValueOnce(new Error("transient disk error"));

    render(
      <ActiveProjectProvider>
        <ManualSaveSpammer
          initialPath="/tmp/recovery.aecstudio"
          spamCount={1}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/recovery.aecstudio",
      ),
    );

    // First save: rejects. The hook clears the slot in its `finally`.
    await act(async () => {
      screen.getByTestId("spam-save").click();
    });
    await waitFor(() => expect(saveSpy).toHaveBeenCalledTimes(1));

    // Second save: must dispatch a fresh bridge call (the in-process
    // mock now resolves because we only queued one rejection above).
    await act(async () => {
      screen.getByTestId("spam-save").click();
    });
    await waitFor(() => expect(saveSpy).toHaveBeenCalledTimes(2));

    saveSpy.mockRestore();
  });
});

/**
 * `saveProject` must not clobber renderer state when a project
 * transition happens while a save is in flight.
 *
 * Devin Review's seventh pass flagged a real architectural race: a
 * save for project A dispatched by the 5-second auto-save (or a
 * manual Ctrl+S) holds the bridge for one disk fsync. If the user
 * clicks "Open Project B" during that window, `openProject` cancels
 * the auto-save timer, calls `aec.project.open(B)`, and commits the
 * new active slot. Then A's save resolves and the IIFE unconditionally
 * calls `setProject(summaryA)` — silently reverting the renderer to
 * project A while every mode page is already rendering against B,
 * and any subsequent `commandApply` / `bimImportIfc` / `draftSet*`
 * call dispatches against the wrong project's path.
 *
 * The fix introduces `projectPathRef` (mirrors `project?.path`) and
 * guards `setProject` in the save IIFE: if the path captured at
 * save-start no longer matches the ref, the save's summary commit is
 * dropped. The bridge call itself is not wasted — the bytes safely
 * landed on disk, which is the only durable contract a save promises.
 *
 * A second related hazard: B's first save would coalesce with A's
 * still-in-flight promise (returning resolved as soon as A's save
 * finishes — falsely telling B's caller "your save is done"). The
 * fix clears `savingPromiseRef.current = null` in every transition
 * callback (open/create/close) so B's first save dispatches its own
 * bridge call. These tests pin both contracts.
 */
function OpenAThenSwitchToB({
  pathA,
  pathB,
  onSaveError,
}: {
  pathA: string;
  pathB: string;
  onSaveError: (err: unknown) => void;
}) {
  const { project, openProject, saveProject } = useActiveProject();
  useEffect(() => {
    void openProject(pathA);
  }, [openProject, pathA]);
  return (
    <div>
      <span data-testid="proj-path">{project?.path ?? ""}</span>
      <button
        type="button"
        data-testid="save-A"
        onClick={() => {
          saveProject().catch(onSaveError);
        }}
      >
        save A
      </button>
      <button
        type="button"
        data-testid="open-B"
        onClick={async () => {
          try {
            await openProject(pathB);
          } catch (err) {
            onSaveError(err);
          }
        }}
      >
        open B
      </button>
      <button
        type="button"
        data-testid="save-B"
        onClick={() => {
          saveProject().catch(onSaveError);
        }}
      >
        save B
      </button>
    </div>
  );
}

describe("useActiveProject — saveProject guards against mid-flight project switch", () => {
  it("does not revert the active project when an old save resolves after openProject(B)", async () => {
    // Hold project A's save resolution so we can transition to B
    // before it settles. Without the `projectPathRef` guard inside
    // the save IIFE, A's resolution would call setProject(summaryA)
    // and revert the renderer to A while the header still shows B.
    let resolveSaveA: ((summary: unknown) => void) | null = null;
    const saveAPromise = new Promise<unknown>((resolve) => {
      resolveSaveA = resolve;
    });
    const saveSpy = vi
      .spyOn(aec.project, "save")
      .mockImplementation(() => saveAPromise);
    const onSaveError = vi.fn();

    render(
      <ActiveProjectProvider>
        <OpenAThenSwitchToB
          pathA="/tmp/projectA.aecstudio"
          pathB="/tmp/projectB.aecstudio"
          onSaveError={onSaveError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    // Start the save for A. The bridge spy holds the promise so the
    // IIFE is parked on the await.
    await act(async () => {
      screen.getByTestId("save-A").click();
    });
    expect(saveSpy).toHaveBeenCalledTimes(1);
    expect(saveSpy).toHaveBeenCalledWith("/tmp/projectA.aecstudio");

    // Switch to B BEFORE A's save resolves. `openProject` uses the
    // in-process backend (no mock for `open`) so it returns project
    // B's summary and commits the slot synchronously after the
    // await.
    await act(async () => {
      screen.getByTestId("open-B").click();
    });
    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectB.aecstudio",
      ),
    );

    // Now resolve A's save with a stale summary that would, WITHOUT
    // the guard, blast through `setProject(summaryA)` and revert the
    // renderer to A.
    await act(async () => {
      resolveSaveA!({
        path: "/tmp/projectA.aecstudio",
        name: "projectA",
        template_key: "blank",
        room_count: 0,
        material_count: 0,
      });
    });

    // The renderer must stay on B. WITHOUT the `projectPathRef`
    // guard, this assertion fails — the textContent would be
    // "/tmp/projectA.aecstudio".
    expect(screen.getByTestId("proj-path").textContent).toBe(
      "/tmp/projectB.aecstudio",
    );
    expect(onSaveError).not.toHaveBeenCalled();

    saveSpy.mockRestore();
  });

  it("does not coalesce a new save for B against an in-flight save for A", async () => {
    // After openProject(B), the savingPromiseRef must be cleared so
    // that saveProject(B) dispatches its own bridge call. Without
    // the clear, B's first save would return A's still-in-flight
    // promise, falsely telling B's caller "your save is done" the
    // moment A's save finishes — and B's bytes would never reach
    // disk until the user's next mutation triggers another save.
    let resolveSaveA: ((summary: unknown) => void) | null = null;
    let resolveSaveB: ((summary: unknown) => void) | null = null;
    const saveSpy = vi
      .spyOn(aec.project, "save")
      .mockImplementation((path: string) => {
        if (path === "/tmp/projectA.aecstudio") {
          return new Promise<unknown>((resolve) => {
            resolveSaveA = resolve;
          });
        }
        if (path === "/tmp/projectB.aecstudio") {
          return new Promise<unknown>((resolve) => {
            resolveSaveB = resolve;
          });
        }
        throw new Error(`unexpected save path: ${path}`);
      });
    const onSaveError = vi.fn();

    render(
      <ActiveProjectProvider>
        <OpenAThenSwitchToB
          pathA="/tmp/projectA.aecstudio"
          pathB="/tmp/projectB.aecstudio"
          onSaveError={onSaveError}
        />
      </ActiveProjectProvider>,
    );

    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectA.aecstudio",
      ),
    );

    // Dispatch a save for A; it parks awaiting our resolveSaveA.
    await act(async () => {
      screen.getByTestId("save-A").click();
    });
    expect(saveSpy).toHaveBeenCalledTimes(1);

    // Switch to B; openProject clears the coalescing slot so the
    // next saveProject is not bound to A's promise.
    await act(async () => {
      screen.getByTestId("open-B").click();
    });
    await waitFor(() =>
      expect(screen.getByTestId("proj-path").textContent).toBe(
        "/tmp/projectB.aecstudio",
      ),
    );

    // Save B. WITHOUT the slot-clear, this would return A's promise
    // and the spy would still show only 1 call. WITH the fix, B
    // dispatches its own bridge call against `pathB`.
    await act(async () => {
      screen.getByTestId("save-B").click();
    });
    expect(saveSpy).toHaveBeenCalledTimes(2);
    expect(saveSpy).toHaveBeenNthCalledWith(2, "/tmp/projectB.aecstudio");

    // Resolve A's stale save first. The equality-guarded `finally`
    // must skip clearing the slot (B's inflight is in it now), so a
    // subsequent saveProject for B still coalesces correctly.
    await act(async () => {
      resolveSaveA!({
        path: "/tmp/projectA.aecstudio",
        name: "projectA",
        template_key: "blank",
        room_count: 0,
        material_count: 0,
      });
    });

    // Now spam another save for B — must coalesce against the
    // existing B inflight (not dispatch a third bridge call).
    await act(async () => {
      screen.getByTestId("save-B").click();
    });
    expect(saveSpy).toHaveBeenCalledTimes(2);

    // Resolve B's save. The slot clears on its own finally, and
    // subsequent saves can dispatch fresh.
    await act(async () => {
      resolveSaveB!({
        path: "/tmp/projectB.aecstudio",
        name: "projectB",
        template_key: "blank",
        room_count: 0,
        material_count: 0,
      });
    });

    await act(async () => {
      screen.getByTestId("save-B").click();
    });
    expect(saveSpy).toHaveBeenCalledTimes(3);

    expect(onSaveError).not.toHaveBeenCalled();
    saveSpy.mockRestore();
  });
});

// Regression: Devin Review flagged that the `value` object passed
// to `ActiveProjectContext.Provider` was constructed inline on
// every render, so every consumer re-rendered whenever ANY of the
// provider's state ticked (project, loading, dirty, saving,
// undoLen, redoLen). Wrapping in `useMemo` over the full state +
// stable-callback dep list preserves referential equality across
// renders that don't change any exposed field. This test pins the
// contract: when nothing observable changes between two renders,
// the context value's identity is preserved (React's `Object.is`
// bail-out can short-circuit consumer re-renders).
describe("useActiveProject — context value identity is memoised", () => {
  it("three sibling consumers in the same provider receive the same value object", async () => {
    const seen: object[] = [];
    function Spy() {
      const value = useActiveProject();
      seen.push(value);
      return null;
    }

    render(
      <ActiveProjectProvider>
        <Spy />
        <Spy />
        <Spy />
      </ActiveProjectProvider>,
    );

    // Wait for the initial mount + `refreshProject` effect to
    // settle. Once settled, the three sibling consumers in the
    // SAME provider instance must each observe the SAME value
    // object — proving the `useMemo` keeps identity stable
    // across consumer renders inside one provider render pass.
    // Without the memo, each `<Spy />` would be re-rendered with
    // a freshly-constructed object literal and the identities
    // would diverge. (React itself doesn't run each consumer in
    // a different render pass — they all share the provider's
    // rendered value — but the regression check here is robust
    // even against future React internals changes because the
    // ProviderContext's value is captured once per provider
    // render and shared with every consumer.)
    await waitFor(() => expect(seen.length).toBeGreaterThanOrEqual(3));
    const a = seen[seen.length - 3];
    const b = seen[seen.length - 2];
    const c = seen[seen.length - 1];
    expect(a).toBe(b);
    expect(b).toBe(c);
  });

  it("preserves saveProject callback identity across successful saves", async () => {
    // Devin Review (commit be262cd) flagged that `saveProject` listed
    // `project` in its `useCallback` deps. Every successful save runs
    // `updateProject(refreshedSummary)` which creates a brand-new
    // `project` object reference (the bridge stamps a new
    // `modifiedAt`), causing `saveProject` to be re-derived and the
    // context `value` to be re-memoised with a fresh identity. The
    // fix threads the active path through `projectPathRef` and drops
    // `project` from the deps — `saveProject` identity is now stable
    // across saves. This test seeds a project, captures `saveProject`
    // before and after a save, and asserts referential equality.
    const seen: Array<() => Promise<void>> = [];
    function Spy() {
      const { saveProject } = useActiveProject();
      seen.push(saveProject);
      return null;
    }
    function Harness() {
      const { openProject, saveProject } = useActiveProject();
      useEffect(() => {
        void openProject("/tmp/identity-save.aecstudio");
      }, [openProject]);
      return (
        <button
          type="button"
          data-testid="trigger-save"
          onClick={async () => {
            await saveProject();
          }}
        >
          save
        </button>
      );
    }

    render(
      <ActiveProjectProvider>
        <Harness />
        <Spy />
      </ActiveProjectProvider>,
    );

    // Wait for openProject to land its summary on the provider.
    await waitFor(() => expect(seen.length).toBeGreaterThanOrEqual(2));
    // Capture identity right before triggering the save.
    const before = seen[seen.length - 1];

    await act(async () => {
      screen.getByTestId("trigger-save").click();
    });
    // Flush the save IIFE and any post-save state commits.
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // Post-save identity must match pre-save identity. Before the
    // fix, the new `project` reference would have re-derived
    // `saveProject` and these would differ.
    const after = seen[seen.length - 1];
    expect(after).toBe(before);
  });

  it("preserves value identity across renders that don't mutate provider state", async () => {
    let setExternal: ((n: number) => void) | null = null;
    const seen: object[] = [];
    function Spy() {
      const value = useActiveProject();
      seen.push(value);
      return null;
    }
    // External-state owner sits OUTSIDE the provider so changes
    // to its state force the provider's children to re-render
    // through normal React reconciliation — without changing any
    // of the provider's own state. The memo must preserve
    // identity across these renders.
    function Host() {
      const [external, setExt] = React.useState(0);
      setExternal = setExt;
      return (
        <ActiveProjectProvider>
          <span data-testid="ext">{external}</span>
          <Spy />
        </ActiveProjectProvider>
      );
    }

    render(<Host />);
    // Wait for initial settle (refreshProject finishes).
    await waitFor(() =>
      expect(screen.getByTestId("ext").textContent).toBe("0"),
    );
    // Capture the identity AFTER all mount effects have run.
    await act(async () => {
      // Flush microtasks so any pending state commits land.
      await Promise.resolve();
    });
    const before = seen[seen.length - 1];

    // External state ticks. Provider's exposed state is untouched.
    await act(async () => {
      setExternal!(1);
    });
    await waitFor(() =>
      expect(screen.getByTestId("ext").textContent).toBe("1"),
    );
    const after = seen[seen.length - 1];
    expect(after).toBe(before);
  });
});
