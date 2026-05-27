import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Bim } from "../pages/Bim";
import { ActiveProjectProvider, useActiveProject } from "../hooks/useActiveProject";
import { ToastProvider, ToastContainer } from "../hooks/useToast";
import { aec } from "../api/aec";
import { useEffect } from "react";

function renderBim() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Bim />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("Bim page", () => {
  it("assembles toolbar, spatial tree, viewport, property editor, schedule, validator", () => {
    renderBim();
    expect(screen.getByTestId("bim-mode")).toBeInTheDocument();
    expect(screen.getByTestId("bim-toolbar")).toBeInTheDocument();
    expect(screen.getByTestId("spatial-tree")).toBeInTheDocument();
    expect(screen.getByTestId("bim-viewport")).toBeInTheDocument();
    expect(screen.getByTestId("property-editor")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-view")).toBeInTheDocument();
    expect(screen.getByTestId("validator-panel")).toBeInTheDocument();
  });

  it("selecting a spatial node opens the property editor for it", () => {
    renderBim();
    fireEvent.click(screen.getByTestId("spatial-node-lvl_l1"));
    expect(
      screen.getByTestId("property-editor").textContent,
    ).toContain("lvl_l1");
  });
});

// Regression: Devin Review flagged that the Bim page's "Classify"
// action invoked `aec.bim.classify({ entityId, source: "ai" })`,
// but the Rust bridge requires `projectPath` and `scheme` (it
// classifies the whole project graph, not a single entity). The
// fix wires `projectPath` from `useActiveProject()` and defaults
// `scheme` to `"ifc"`, guarding against the no-project case with
// a toast. These tests pin both the guard and the call shape.
function BimWithProject({ path }: { path: string | null }) {
  const { project, openProject } = useActiveProject();
  useEffect(() => {
    if (path) {
      void openProject(path);
    }
  }, [path, openProject]);
  return (
    <>
      {/* Surface the active project path for the test to wait on,
          since `Bim` itself doesn't render the project name. */}
      <span data-testid="active-project-path">
        {project?.path ?? "no-project"}
      </span>
      <Bim />
    </>
  );
}

describe("Bim page — classify action", () => {
  // Capture the precise spy types by initialising once and then
  // resetting per-test. A `ReturnType<typeof vi.spyOn>` widens to
  // `MockInstance<unknown[], unknown>` which won't accept the
  // typed `mockResolvedValue` payload below.
  let classifySpy = vi.spyOn(aec.bim, "classify");
  let currentSpy = vi.spyOn(aec.project, "current");
  let openSpy = vi.spyOn(aec.project, "open");
  classifySpy.mockRestore();
  currentSpy.mockRestore();
  openSpy.mockRestore();

  beforeEach(() => {
    classifySpy = vi.spyOn(aec.bim, "classify");
    // The in-process fallback shares a `recents` singleton across
    // tests in the same vitest worker, so `aec.project.current()`
    // may return a project created by an earlier test. We stub
    // both `current` (to control the initial "no project" state)
    // and `open` (to control which project gets opened) so each
    // test owns its world.
    currentSpy = vi.spyOn(aec.project, "current");
    openSpy = vi.spyOn(aec.project, "open");
  });

  afterEach(() => {
    classifySpy.mockRestore();
    currentSpy.mockRestore();
    openSpy.mockRestore();
  });

  it("blocks classify with a toast when no project is open (no IPC call)", async () => {
    currentSpy.mockResolvedValue({ summary: null });
    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <Bim />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );
    // Wait for the initial `refreshProject` to settle.
    await waitFor(() => {
      expect(currentSpy).toHaveBeenCalled();
    });
    fireEvent.click(screen.getByTestId("bim-action-classify"));
    // No project → bridge never called.
    await waitFor(() => {
      expect(
        screen.getByText(/No project open/i),
      ).toBeInTheDocument();
    });
    expect(classifySpy).not.toHaveBeenCalled();
  });

  it("invokes classify with projectPath + scheme:ifc when a project is open", async () => {
    const summary = {
      projectId: "proj_sample",
      name: "Sample",
      path: "/tmp/sample.aecstudio",
      templateKey: null,
      modifiedAt: new Date().toISOString(),
    };
    // Set both `current` (read on provider mount) and `open` to the
    // same summary so the initial `refreshProject()` and the test's
    // explicit `openProject()` agree on the active path — otherwise
    // refreshProject's resolved `null` races openProject's resolved
    // summary in the microtask queue and the last write wins.
    currentSpy.mockResolvedValue({ summary });
    openSpy.mockResolvedValue(summary);
    classifySpy.mockResolvedValue({
      scheme: "ifc",
      classified: 3,
      unchanged: 1,
      skipped: 0,
      details: [],
    });
    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <BimWithProject path="/tmp/sample.aecstudio" />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );
    // Wait for the active-project state to reflect the opened
    // path; this is the precondition for the classify branch to
    // see a non-null `projectPath` when the toolbar fires.
    await waitFor(() => {
      expect(
        screen.getByTestId("active-project-path").textContent,
      ).toBe("/tmp/sample.aecstudio");
    });
    fireEvent.click(screen.getByTestId("bim-action-classify"));
    await waitFor(() => {
      expect(classifySpy).toHaveBeenCalledWith({
        projectPath: "/tmp/sample.aecstudio",
        scheme: "ifc",
      });
    });
    // Toast surfaces counts so the user sees the operation result.
    await waitFor(() => {
      expect(
        screen.getByText(/Classified 3 entities/i),
      ).toBeInTheDocument();
    });
  });
});

// Regression: Devin Review flagged that `onInvoke` in `Bim.tsx`
// had a `try/finally` but no `catch`. Pre-Phase 13 every BIM
// branch targeted `demo://...` paths handled by the in-process
// fallback (which never throws), so the missing catch was
// benign. Phase 13 wires every branch through real OS paths from
// the file-picker, so bridge failures (disk full, permission
// denied, malformed IFC, locked SQLCipher DB) are realistic and
// must surface as a user-visible error toast — silent unhandled
// promise rejections clear the busy spinner with no feedback.
// The fix is a single `catch` after the switch that converts any
// uncaught bridge error into an `error`-severity toast. These
// tests pin that contract for the `classify` branch (the same
// catch covers `exportIfc` / `validate` / `diff` /
// `generateSchedule` / `boq` uniformly — one test is sufficient
// because the catch is shared, and adding five duplicates would
// only test the spy harness).
describe("Bim page — bridge error handling", () => {
  let classifySpy = vi.spyOn(aec.bim, "classify");
  let currentSpy = vi.spyOn(aec.project, "current");
  let openSpy = vi.spyOn(aec.project, "open");
  classifySpy.mockRestore();
  currentSpy.mockRestore();
  openSpy.mockRestore();

  beforeEach(() => {
    classifySpy = vi.spyOn(aec.bim, "classify");
    currentSpy = vi.spyOn(aec.project, "current");
    openSpy = vi.spyOn(aec.project, "open");
  });

  afterEach(() => {
    classifySpy.mockRestore();
    currentSpy.mockRestore();
    openSpy.mockRestore();
  });

  it("surfaces an error toast when the bridge rejects mid-operation", async () => {
    const summary = {
      projectId: "proj_sample",
      name: "Sample",
      path: "/tmp/sample.aecstudio",
      templateKey: null,
      modifiedAt: new Date().toISOString(),
    };
    currentSpy.mockResolvedValue({ summary });
    openSpy.mockResolvedValue(summary);
    classifySpy.mockRejectedValue(new Error("SQLCipher database is locked"));

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <BimWithProject path="/tmp/sample.aecstudio" />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    await waitFor(() => {
      expect(
        screen.getByTestId("active-project-path").textContent,
      ).toBe("/tmp/sample.aecstudio");
    });

    fireEvent.click(screen.getByTestId("bim-action-classify"));

    // The error toast must include the action name and the
    // bridge's error message so the user can diagnose the cause
    // (locked DB, disk full, etc.) without having to open
    // devtools.
    await waitFor(() => {
      expect(
        screen.getByText(/classify failed: SQLCipher database is locked/i),
      ).toBeInTheDocument();
    });

    // The spy was called (the catch is post-failure, not
    // pre-empting the call).
    expect(classifySpy).toHaveBeenCalledTimes(1);
  });
});

// Regression: Devin Review flagged that the Bim page's
// `ifcSourcePath` is per-session local state, not keyed to the
// active project — if the component stayed mounted across a
// project switch, the user would silently validate / export /
// classify against project A's IFC while viewing project B.
// Today this is hidden by the `RequireProject` route guard +
// every project-switch path navigating away from `/bim` (so the
// component unmounts and `useState` resets for free), but the
// invariant is fragile: an in-page project picker or any future
// flow that calls `openProject` without navigating would silently
// break the contract. The fix is a `useEffect` keyed on
// `project?.path` that mirrors the pattern `Render.tsx` already
// uses (resets `cameras`/`selectedCameras`) and resets every
// per-project Bim state slot. These tests pin the contract by
// driving a project switch on a still-mounted Bim instance and
// verifying that the ScheduleView's regenerate button — whose
// `disabled` state directly mirrors `ifcSourcePath === null` —
// transitions enabled → disabled across the switch.
describe("Bim page — per-project state reset on project switch", () => {
  let openFileSpy = vi.spyOn(aec.dialog, "openFile");
  let attachIfcSpy = vi.spyOn(aec.bim, "attachIfc");
  let currentSpy = vi.spyOn(aec.project, "current");
  let openSpy = vi.spyOn(aec.project, "open");
  openFileSpy.mockRestore();
  attachIfcSpy.mockRestore();
  currentSpy.mockRestore();
  openSpy.mockRestore();

  beforeEach(() => {
    openFileSpy = vi.spyOn(aec.dialog, "openFile");
    attachIfcSpy = vi.spyOn(aec.bim, "attachIfc");
    currentSpy = vi.spyOn(aec.project, "current");
    openSpy = vi.spyOn(aec.project, "open");
  });

  afterEach(() => {
    openFileSpy.mockRestore();
    attachIfcSpy.mockRestore();
    currentSpy.mockRestore();
    openSpy.mockRestore();
  });

  it("clears ifcSourcePath when the active project changes (re-mounted Bim instance)", async () => {
    const summaryA = {
      projectId: "proj_a",
      name: "Project A",
      path: "/tmp/a.aecstudio",
      templateKey: null,
      modifiedAt: new Date().toISOString(),
    };
    const summaryB = {
      projectId: "proj_b",
      name: "Project B",
      path: "/tmp/b.aecstudio",
      templateKey: null,
      modifiedAt: new Date().toISOString(),
    };
    // `current` is read once on provider mount. `open` is driven
    // by `BimWithProject`'s `useEffect` on `path` change — the
    // implementation maps each `path` to the matching summary so
    // a re-render with a different `path` switches projects
    // without unmounting the Bim component.
    currentSpy.mockResolvedValue({ summary: summaryA });
    openSpy.mockImplementation(async (path: string) =>
      path === summaryA.path ? summaryA : summaryB,
    );
    // Drive the attach flow: dialog returns a deterministic IFC
    // path, the bridge returns a successful attach summary, and
    // the Bim component sets `ifcSourcePath` to the picked path
    // (which is the precondition for the regenerate button to
    // become enabled).
    openFileSpy.mockResolvedValue({
      canceled: false,
      paths: ["/tmp/site.ifc"],
    });
    attachIfcSpy.mockResolvedValue({
      path: "/tmp/site.ifc",
      projectPath: summaryA.path,
      parseCacheHit: false,
      spatialNodesInserted: 1,
      spatialNodesUpdated: 0,
      spatialNodesUnchanged: 0,
      elementsInserted: 5,
      elementsUpdated: 0,
      elementsUnchanged: 0,
      componentsInserted: 0,
      relationsInserted: 0,
      cacheRows: 0,
    });

    const { rerender } = render(
      <ToastProvider>
        <ActiveProjectProvider>
          <BimWithProject path={summaryA.path} />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for project A to be the active project.
    await waitFor(() => {
      expect(
        screen.getByTestId("active-project-path").textContent,
      ).toBe(summaryA.path);
    });

    // Initially the regenerate button is disabled — no IFC has
    // been imported / attached yet.
    expect(screen.getByTestId("schedule-regenerate")).toBeDisabled();

    // Trigger the attach action, which routes through the
    // file-picker + bridge spies and sets `ifcSourcePath`.
    fireEvent.click(screen.getByTestId("bim-action-attachIfc"));

    // Regenerate becomes enabled once `ifcSourcePath` is set.
    await waitFor(() => {
      expect(screen.getByTestId("schedule-regenerate")).toBeEnabled();
    });

    // Switch to project B by re-rendering with the new path.
    // The `BimWithProject` useEffect fires `openProject(summaryB.path)`
    // which transitions the provider state; the Bim component's
    // new `useEffect` keyed on `project?.path` then resets
    // `ifcSourcePath` to null.
    rerender(
      <ToastProvider>
        <ActiveProjectProvider>
          <BimWithProject path={summaryB.path} />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for the active project to switch to B.
    await waitFor(() => {
      expect(
        screen.getByTestId("active-project-path").textContent,
      ).toBe(summaryB.path);
    });

    // The regenerate button must be disabled again — confirming
    // that `ifcSourcePath` was reset by the project-switch
    // useEffect. If this assertion ever fails, project A's IFC
    // path would silently leak into operations performed under
    // project B (the exact bug Devin Review flagged).
    await waitFor(() => {
      expect(screen.getByTestId("schedule-regenerate")).toBeDisabled();
    });
  });
});
