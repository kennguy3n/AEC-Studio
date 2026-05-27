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
