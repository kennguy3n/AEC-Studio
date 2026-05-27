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
 */

import { describe, expect, it, vi } from "vitest";
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
