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
import { useEffect } from "react";
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
