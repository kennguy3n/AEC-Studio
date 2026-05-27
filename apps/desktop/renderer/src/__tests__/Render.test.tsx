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
});
