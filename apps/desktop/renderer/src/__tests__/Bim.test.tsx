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
