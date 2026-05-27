import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Render } from "../pages/Render";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";
import { aec } from "../api/aec";
import { useEffect } from "react";

function renderPage() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Render />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("Render page", () => {
  it("assembles preset, cameras, queue, doctor, preview, compare", () => {
    renderPage();
    expect(screen.getByTestId("render-mode")).toBeInTheDocument();
    expect(screen.getByTestId("preset-selector")).toBeInTheDocument();
    expect(screen.getByTestId("camera-selector")).toBeInTheDocument();
    expect(screen.getByTestId("render-queue")).toBeInTheDocument();
    expect(screen.getByTestId("render-doctor")).toBeInTheDocument();
    expect(screen.getByTestId("render-preview")).toBeInTheDocument();
    expect(screen.getByTestId("before-after-compare")).toBeInTheDocument();
  });
});

/**
 * Regression: project-switch must clear the camera list *and* the
 * `selectedCameras` set, not just the visible tile list. Camera IDs
 * are project-scoped — a stale ID left in the selection set would
 * cause `Queue renders` to enqueue an orphaned render job that the
 * native render queue would reject (or, worse, silently address the
 * wrong entity in a project that happened to share an ID).
 *
 * The test seeds two projects with disjoint camera-ID sets via a
 * mocked `aec.command.listGraph`, selects a camera in project A,
 * switches to project B, and asserts that:
 *   1. Project A's selected `data-testid="camera-tile-<id>"` tile is
 *      no longer in the DOM (cameras list reset to project B's).
 *   2. Project B's cameras render with `aria-pressed="false"`
 *      (selection set was cleared, not silently carried over).
 */
function OpenSwitch({
  pathA,
  pathB,
}: {
  pathA: string;
  pathB: string;
}) {
  const { openProject } = useActiveProject();
  useEffect(() => {
    void openProject(pathA);
  }, [openProject, pathA]);
  return (
    <button
      type="button"
      data-testid="switch-to-B"
      onClick={() => {
        void openProject(pathB);
      }}
    >
      switch
    </button>
  );
}

describe("Render page — project switch resets camera selection", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears selectedCameras when project?.path changes", async () => {
    const PATH_A = "/tmp/render-A.aecstudio";
    const PATH_B = "/tmp/render-B.aecstudio";

    // Mock `command.listGraph` to return disjoint camera entities
    // depending on which project's path is queried. The actual
    // in-process backend would require seeding via `command.apply`
    // with create-camera commands — mocking here is the minimum-
    // surface choice that exercises the page-level fix.
    vi.spyOn(aec.command, "listGraph").mockImplementation(
      async (path: string, kind?: string) => {
        if (kind !== "camera") return [];
        if (path === PATH_A) {
          return [
            {
              id: "cam_A_1",
              kind: "camera",
              parent: null,
              body: { name: "A1" },
            },
            {
              id: "cam_A_2",
              kind: "camera",
              parent: null,
              body: { name: "A2" },
            },
          ];
        }
        if (path === PATH_B) {
          return [
            {
              id: "cam_B_1",
              kind: "camera",
              parent: null,
              body: { name: "B1" },
            },
          ];
        }
        return [];
      },
    );

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <OpenSwitch pathA={PATH_A} pathB={PATH_B} />
          <Render />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for project A's cameras to load.
    await waitFor(() => {
      expect(screen.getByTestId("camera-tile-cam_A_1")).toBeInTheDocument();
      expect(screen.getByTestId("camera-tile-cam_A_2")).toBeInTheDocument();
    });

    // Select project A's first camera.
    fireEvent.click(screen.getByTestId("camera-toggle-cam_A_1"));
    expect(
      screen.getByTestId("camera-toggle-cam_A_1").getAttribute("aria-pressed"),
    ).toBe("true");

    // Switch to project B.
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
    });

    // Project A's tiles must be gone.
    await waitFor(() => {
      expect(screen.queryByTestId("camera-tile-cam_A_1")).toBeNull();
      expect(screen.queryByTestId("camera-tile-cam_A_2")).toBeNull();
      expect(screen.getByTestId("camera-tile-cam_B_1")).toBeInTheDocument();
    });

    // Project B's camera must NOT inherit the previous selection.
    expect(
      screen.getByTestId("camera-toggle-cam_B_1").getAttribute("aria-pressed"),
    ).toBe("false");
  });

  it("clears the visible jobs queue synchronously on project switch — no flash of project A's jobs in project B", async () => {
    // Devin Review flagged a visual flash on project switch: the
    // `cameras` and `selectedCameras` reset was synchronous, but the
    // `jobs` reset waited for the new project's `listJobs()` promise
    // to resolve. The microtask gap was brief but real — long enough
    // to render project A's jobs (with project-A jobIds) in project
    // B's queue, which would also break any `cancelJob` / `diagnose`
    // call that subsequently keyed off those stale IDs (the native
    // queue would reject the cancel for a job that isn't in project
    // B's scope).
    //
    // The fix adds `setJobs([])` alongside the camera reset. This
    // test pins the contract: between firing the project switch and
    // the next listJobs promise resolving, project A's job rows must
    // NOT be in the DOM (proving the synchronous reset ran before
    // any await yielded). We block the second `listJobs` call with
    // a promise that never resolves during the assertion window —
    // if the reset depended on the async path, project A's jobs
    // would still be visible.
    const PATH_A = "/tmp/render-jobs-A.aecstudio";
    const PATH_B = "/tmp/render-jobs-B.aecstudio";

    let resolveB: (jobs: unknown[]) => void = () => {};
    const bGate = new Promise<unknown[]>((resolve) => {
      resolveB = resolve;
    });

    // Sequence of `listJobs` calls:
    //   1. Initial mount with no project — returns empty.
    //   2. After `openProject(PATH_A)` runs — returns project A's job.
    //   3. After `openProject(PATH_B)` runs — blocks indefinitely so
    //      the synchronous `setJobs([])` reset is observable between
    //      the project-switch commit and the bridge response landing.
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockImplementation(async () => {
        const callIndex = listJobsSpy.mock.calls.length;
        if (callIndex === 1) return [];
        if (callIndex === 2) {
          return [
            {
              jobId: "jobA1",
              cameraId: "cam_A_1",
              progress: 0.5,
              status: "running",
              tier: "Workstation",
              preset: "interior_balanced",
              tiles: { completed: 1, total: 2 },
            },
          ];
        }
        return bGate;
      });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <OpenSwitch pathA={PATH_A} pathB={PATH_B} />
          <Render />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Project A's job row appears once listJobs resolves.
    await waitFor(() => {
      expect(screen.getByTestId("render-job-jobA1")).toBeInTheDocument();
    });

    // Switch to project B. The component-level useEffect runs
    // synchronously; `setJobs([])` should fire BEFORE the next
    // listJobs() resolves (which is gated indefinitely here).
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
    });

    // Project A's job must be gone — the synchronous reset cleared
    // it before the async fetch had a chance to repopulate.
    expect(screen.queryByTestId("render-job-jobA1")).toBeNull();

    // The queue must show its empty state, not project A's stale row.
    expect(screen.getByTestId("render-queue").textContent).toContain(
      "No jobs queued.",
    );

    // Resolve the gate so the test doesn't leak the pending promise.
    resolveB([]);
    await act(async () => {
      await Promise.resolve();
    });

    listJobsSpy.mockRestore();
  });
});
