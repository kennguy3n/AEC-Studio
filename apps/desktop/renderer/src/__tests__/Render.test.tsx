import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Render } from "../pages/Render";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";
import { ToastProvider, ToastContainer } from "../hooks/useToast";
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
    //   1. After `openProject(PATH_A)` runs — returns project A's job.
    //   2. After `openProject(PATH_B)` runs — blocks indefinitely so
    //      the synchronous `setJobs([])` reset is observable between
    //      the project-switch commit and the bridge response landing.
    //
    // (Initial mount with `project === null` does NOT call listJobs —
    // the bridge fetch is gated on `project?.path` in the page effect,
    // matching the `StatusBar.tsx:62-93` polling-gate pattern. With no
    // active project there's no DB to address, so issuing the call
    // would be pure IPC waste.)
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockImplementation(async () => {
        const callIndex = listJobsSpy.mock.calls.length;
        if (callIndex === 1) {
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

/**
 * Devin Review (commit be262cd) flagged that the Render page's main
 * `useEffect` listed `project?.path` in its deps AND issued the
 * `aec.runtime.status()` IPC inside the same effect. That call
 * fetches static hardware/AI tier info (CPU cores, GPU, RAM) — the
 * value cannot change while the renderer process is running (hot-
 * plugging a GPU mid-session requires a full app restart). Listing
 * it under `[project?.path]` re-issued the IPC on every project
 * transition for zero observable benefit. The fix splits the call
 * into its own mount-only `useEffect([], [])`, matching the pattern
 * `StatusBar.tsx:16-27` already uses. This test pins the contract:
 * one `runtime.status()` call across N project transitions.
 */
describe("Render page — runtime.status is mount-only", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("does NOT re-issue aec.runtime.status() on project transitions", async () => {
    const PATH_A = "/tmp/render-rs-A.aecstudio";
    const PATH_B = "/tmp/render-rs-B.aecstudio";

    const runtimeStatusSpy = vi
      .spyOn(aec.runtime, "status")
      .mockResolvedValue({
        tier: "Workstation",
        cpuName: "Test CPU",
        gpuName: "Test GPU",
        ramGb: 32,
      });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <OpenSwitch pathA={PATH_A} pathB={PATH_B} />
          <Render />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for the initial runtime.status() call to land.
    await waitFor(() => {
      expect(runtimeStatusSpy).toHaveBeenCalledTimes(1);
    });

    // Switch to project B. Project transition fires the per-project
    // effect (cameras / jobs reset + listGraph), but the mount-only
    // effect must NOT re-run.
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // After the project transition: still exactly one
    // runtime.status() call. The hardware tier is session-scoped, so
    // re-fetching on every project switch is pure IPC waste.
    expect(runtimeStatusSpy).toHaveBeenCalledTimes(1);

    runtimeStatusSpy.mockRestore();
  });
});

/**
 * Devin Review (commit be262cd) flagged that `enqueueAll` wrapped
 * the per-camera loop in `try/finally` with no `catch`. Each call
 * to `aec.render.enqueueRender` is its own IPC round-trip — a single
 * bridge-side rejection (stale camera ID, tier-downgrade rejecting
 * the preset, OOM enqueueing a 4K preset on a Tablet tier) would
 * propagate as an unhandled promise rejection from the `onClick`
 * handler. The user saw the cameras that succeeded land in the
 * queue and the rest silently disappear with no toast, log entry,
 * or any other affordance. The fix wraps each per-camera enqueue
 * in its own `try/catch`, accumulates `{cameraId, message}` failure
 * records, commits whatever succeeded to the queue, and surfaces:
 *   - a success toast for the cameras that landed (if any)
 *   - an error toast for the cameras that failed (if any), with the
 *     camera IDs + bridge error message in the body so the user has
 *     actionable info.
 * This test pins the error-path contract.
 */
describe("Render page — enqueueAll surfaces partial failure", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("commits the cameras that succeeded and surfaces an error toast for the cameras that failed", async () => {
    const PATH_A = "/tmp/render-enqueue-A.aecstudio";

    vi.spyOn(aec.command, "listGraph").mockImplementation(
      async (path: string, kind?: string) => {
        if (kind !== "camera" || path !== PATH_A) return [];
        return [
          { id: "cam_ok", kind: "camera", parent: null, body: { name: "OK" } },
          {
            id: "cam_bad",
            kind: "camera",
            parent: null,
            body: { name: "BAD" },
          },
        ];
      },
    );

    let call = 0;
    const enqueueSpy = vi
      .spyOn(aec.render, "enqueueRender")
      .mockImplementation(async () => {
        call += 1;
        if (call === 1) return { jobId: "job_ok" };
        throw new Error("queue depth exceeded");
      });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <OpenSwitch pathA={PATH_A} pathB={PATH_A} />
          <Render />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for project A's cameras to load.
    await waitFor(() => {
      expect(screen.getByTestId("camera-tile-cam_ok")).toBeInTheDocument();
      expect(screen.getByTestId("camera-tile-cam_bad")).toBeInTheDocument();
    });

    // Select both cameras then trigger the queue.
    fireEvent.click(screen.getByTestId("camera-toggle-cam_ok"));
    fireEvent.click(screen.getByTestId("camera-toggle-cam_bad"));

    await act(async () => {
      fireEvent.click(screen.getByTestId("render-enqueue-all"));
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // The good camera's job ID must appear in the queue — partial
    // success is committed, not rolled back.
    await waitFor(() => {
      expect(screen.getByTestId("render-job-job_ok")).toBeInTheDocument();
    });

    // Both a success toast (for the camera that landed) AND an error
    // toast (for the camera that failed) must render. The error
    // toast body must include the failing camera ID and the bridge
    // error message so the user has actionable info.
    const toasts = screen.getAllByRole("alert").map((n) => n.textContent ?? "");
    const okToast = toasts.find((t) => t.includes("Queued 1 render"));
    const errToast = toasts.find((t) => t.includes("Failed to queue"));
    expect(okToast).toBeDefined();
    expect(errToast).toBeDefined();
    expect(errToast!).toContain("cam_bad");
    expect(errToast!).toContain("queue depth exceeded");

    enqueueSpy.mockRestore();
  });
});

/**
 * Devin Review (commit 31f975e) flagged that the per-project useEffect
 * issued `aec.render.listJobs()` unconditionally on every project
 * transition, including the no-project initial mount where the
 * native bridge has no DB to address and the in-process fallback
 * short-circuits to `[]`. The result was one IPC round-trip per
 * route landing on Home/Settings + a redundant `setJobs([])` commit
 * after the synchronous reset already ran.
 *
 * The fix gates BOTH `listJobs` and `listGraph` on `project?.path`,
 * mirroring the `StatusBar.tsx:62-93` polling-gate pattern. This
 * test pins the contract: with no active project, neither bridge
 * call is issued; the empty state for the queue and the camera grid
 * still renders correctly from the synchronous reset.
 */
describe("Render page — bridge fetches gated on active project", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("does NOT issue listJobs or listGraph on initial mount when no project is active", async () => {
    // Force `project.current()` to report no active project. The
    // in-process backend retains state across vitest tests in the
    // same file (module-level `current` mutable), so earlier tests
    // that opened a project would otherwise leak a non-null summary
    // into this test's initial mount.
    vi.spyOn(aec.project, "current").mockResolvedValue({ summary: null });

    const listJobsSpy = vi.spyOn(aec.render, "listJobs").mockResolvedValue([]);
    const listGraphSpy = vi
      .spyOn(aec.command, "listGraph")
      .mockResolvedValue([]);

    renderPage();

    // Yield enough microtasks to let any latent bridge call resolve.
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    // The page is mounted with `project === null` (route guard would
    // normally redirect, but the harness renders <Render /> directly
    // to exercise the effect's null-project branch). Both bridge
    // fetches must remain unstarted; the queue and camera grid both
    // render their empty-state UI from the synchronous reset.
    expect(listJobsSpy).not.toHaveBeenCalled();
    expect(listGraphSpy).not.toHaveBeenCalled();

    // Empty states are visible (synchronous reset committed an empty
    // cameras list and an empty jobs list; CameraSelector + RenderQueue
    // render their built-in empty-state messages).
    expect(screen.getByTestId("camera-selector")).toHaveClass(
      "render-cameras--empty",
    );
    expect(screen.getByTestId("render-queue")).toHaveClass(
      "render-queue--empty",
    );

    listJobsSpy.mockRestore();
    listGraphSpy.mockRestore();
  });
});

/**
 * Devin Review (commit 31f975e) flagged that `FALLBACK_CAMERAS`
 * (two hard-coded `cam_living` / `cam_kitchen` tiles) persisted
 * whenever `listGraph` returned zero camera entities for the active
 * project. A user on a brand-new project could `toggleCamera` a
 * fake ID and click "Queue renders" — the per-camera
 * `aec.render.enqueueRender` would either fail (bridge rejecting
 * the unknown entity) or silently enqueue an orphaned job that no
 * `diagnose` / `cancelJob` call could subsequently address.
 *
 * The fix removes the fallback entirely. When `listGraph` returns
 * `[]`, `cameras` stays empty (from the synchronous reset), and
 * `CameraSelector` renders its built-in empty-state UI directing
 * the user to "Save current view" in the 3D viewport. This test
 * pins the contract: empty real-project list + zero `enqueueRender`
 * calls because no fake cameras are toggleable.
 */
describe("Render page — empty project shows empty-state, not fake cameras", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows the empty-state message when the active project has zero cameras", async () => {
    const PATH = "/tmp/render-empty.aecstudio";

    vi.spyOn(aec.command, "listGraph").mockResolvedValue([]);
    const enqueueSpy = vi.spyOn(aec.render, "enqueueRender");

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <OpenSwitch pathA={PATH} pathB={PATH} />
          <Render />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for openProject + the per-project effect to commit.
    await waitFor(() => {
      expect(screen.getByTestId("camera-selector")).toHaveClass(
        "render-cameras--empty",
      );
    });

    // Empty-state message must direct the user to the 3D viewport
    // path for creating cameras (which is the architecturally
    // correct way to populate the list — fake demo entries that the
    // user could accidentally enqueue against were the bug).
    expect(screen.getByTestId("camera-selector").textContent).toContain(
      "No saved cameras yet",
    );

    // No fake "cam_living" / "cam_kitchen" tiles in the DOM — they
    // would be the legacy fallback content.
    expect(screen.queryByTestId("camera-tile-cam_living")).toBeNull();
    expect(screen.queryByTestId("camera-tile-cam_kitchen")).toBeNull();

    // The "Queue renders" button cannot be reached because there
    // are no tiles to toggle; the button is disabled at zero
    // selection size. Sanity-check that enqueue was never called.
    expect(enqueueSpy).not.toHaveBeenCalled();

    enqueueSpy.mockRestore();
  });
});

/**
 * Devin Review (commit 29c53ff, finding 3314923605) flagged that the
 * per-project useEffect issued `aec.render.listJobs()` with `.then()`
 * but no `.catch()`, leaving bridge rejections unhandled. Without a
 * catch, a bridge failure (project closed mid-fetch, corrupt render-
 * jobs row, permission denied on the render-store DB) would surface
 * as "Uncaught (in promise)" in the renderer console — a real risk
 * during the routine project-switch race window where the previous
 * project's listJobs call is in flight while the new project's effect
 * has already cleared the active path.
 *
 * The fix adds a silent `.catch()` matching the existing pattern on
 * `listGraph` immediately below (Render.tsx:156) and the
 * `StatusBar.tsx` / `Deliver.tsx` polling-tick convention: empty/
 * last-known UI, no toast (routine project-switch races would
 * otherwise spam transient-failure toasts). The bridge layer logs
 * the underlying error.
 */
describe("Render page — listJobs bridge rejection handled silently", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("swallows aec.render.listJobs() rejection without surfacing an unhandled rejection", async () => {
    const PATH = "/tmp/render-listjobs-reject.aecstudio";

    // Mock listGraph to succeed (no cameras) so its catch path is
    // not exercised — we want to isolate the listJobs catch.
    vi.spyOn(aec.command, "listGraph").mockResolvedValue([]);

    // Mock listJobs to always reject, regardless of how many times
    // the effect fires (initial mount, openProject A, any future
    // re-fire). Counting fires is fragile; mocking every call
    // means an unhandled rejection from any of them would surface.
    vi.spyOn(aec.render, "listJobs").mockRejectedValue(
      new Error("simulated bridge failure"),
    );

    // Capture unhandled rejections so we can assert none surface.
    // Without the catch in Render.tsx, the rejected promise from
    // `aec.render.listJobs()` would surface here.
    const unhandled: unknown[] = [];
    const onUnhandled = (event: PromiseRejectionEvent) => {
      unhandled.push(event.reason);
      // Prevent jsdom from logging the rejection to console — we
      // are intentionally exercising the catch path and recording
      // observations in `unhandled` ourselves.
      event.preventDefault();
    };
    window.addEventListener("unhandledrejection", onUnhandled);

    try {
      render(
        <ToastProvider>
          <ActiveProjectProvider>
            <OpenSwitch pathA={PATH} pathB={PATH} />
            <Render />
          </ActiveProjectProvider>
        </ToastProvider>,
      );

      // Wait for openProject + the per-project effect to commit.
      // The empty-queue state must render (synchronous reset
      // committed an empty jobs list; the rejected listJobs() did
      // NOT overwrite it).
      await waitFor(() => {
        expect(screen.getByTestId("render-queue")).toHaveClass(
          "render-queue--empty",
        );
      });

      // Yield a few microtasks/macrotasks so any queued
      // unhandled-rejection events fire before assertion.
      await new Promise((r) => setTimeout(r, 50));
      await new Promise((r) => setTimeout(r, 50));

      expect(unhandled).toEqual([]);
    } finally {
      window.removeEventListener("unhandledrejection", onUnhandled);
    }
  });
});
