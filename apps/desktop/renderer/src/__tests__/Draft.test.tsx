import { afterEach, describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { Draft } from "../pages/Draft";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";
import { ToastContainer, ToastProvider } from "../hooks/useToast";
import { aec } from "../api/aec";

function renderDraft() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Draft />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("Draft page", () => {
  it("assembles toolbar, canvas, layer panel, inspector, sheet manager, command line", () => {
    renderDraft();
    expect(screen.getByTestId("draft-mode")).toBeInTheDocument();
    expect(screen.getByTestId("draft-tool-line")).toBeInTheDocument();
    expect(screen.getByTestId("draft-canvas")).toBeInTheDocument();
    expect(screen.getByTestId("layer-panel")).toBeInTheDocument();
    expect(screen.getByTestId("draft-inspector")).toBeInTheDocument();
    expect(screen.getByTestId("sheet-manager")).toBeInTheDocument();
    expect(screen.getByTestId("command-line")).toBeInTheDocument();
  });
});

/**
 * Devin Review (commit be262cd) flagged that Draft.tsx maintained
 * `activeTool` / `layers` / `sheets` / `activeSheet` / `log` /
 * `selection` in `useState` with NO `useEffect([project?.path])` to
 * reset them on project transitions. Today this is benign because
 * `RequireProject` unmounts the Draft page on every project switch
 * (so `useState` resets naturally), but the unmount-on-switch rule
 * is a route-guard convention — not a contract the page itself
 * owns. Any future in-page project picker, "Recent project" jump,
 * or any other flow that calls `openProject` without navigating
 * away from `/draft` would silently leak project A's sheets / layers
 * / command log into project B. The fix adds the explicit reset
 * effect mirroring `Bim.tsx:94-110` and `Render.tsx:90-149`. This
 * test pins the contract by switching the active project WITHOUT
 * unmounting the Draft page and asserting that the sheet manager
 * resets to its single-sheet default.
 */
function DraftWithSwitch({
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
    <>
      <button
        type="button"
        data-testid="switch-to-B"
        onClick={() => {
          void openProject(pathB);
        }}
      >
        switch
      </button>
      <Draft />
    </>
  );
}

describe("Draft page — project switch resets per-project state", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears extra sheets when project?.path changes (without unmounting the page)", async () => {
    const PATH_A = "/tmp/draft-A.aecstudio";
    const PATH_B = "/tmp/draft-B.aecstudio";

    // Mock the bridge sheet-create so the new sheet ID is predictable
    // and the await resolves on the next microtask (the in-process
    // backend's `newId` would otherwise stamp a timestamp-based ID
    // that's harder to assert against, but the same mock-on-real-
    // surface convention is established by the existing
    // `Render.test.tsx` `aec.command.listGraph` spy above).
    const createSheetSpy = vi
      .spyOn(aec.draft, "createSheet")
      .mockResolvedValue({ sheetId: "sheet-extra" });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <DraftWithSwitch pathA={PATH_A} pathB={PATH_B} />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for project A to be opened. The default sheet tab is the
    // only one present after mount.
    await waitFor(() => {
      expect(screen.getByTestId("sheet-tab-sheet-default")).toBeInTheDocument();
    });

    // Add a second sheet via the SheetManager's "create" affordance.
    // The create handler goes through the bridge to mint a sheet ID,
    // so wrap in `act` to flush the async state commit, then also
    // flush microtasks so the post-await `onChange` lands.
    await act(async () => {
      fireEvent.click(screen.getByTestId("sheet-create"));
    });
    await waitFor(() => {
      expect(screen.getByTestId("sheet-tab-sheet-extra")).toBeInTheDocument();
    });

    // Switch to project B. The Draft page does NOT unmount (no
    // `RequireProject` wrapper in this harness), so without the
    // per-project reset the extra sheet from project A would leak
    // into project B's view.
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    // After the project transition: the per-project reset effect
    // must have fired, leaving only the default sheet tab. The extra
    // sheet from project A is gone.
    await waitFor(() => {
      expect(screen.getByTestId("sheet-tab-sheet-default")).toBeInTheDocument();
      expect(screen.queryByTestId("sheet-tab-sheet-extra")).toBeNull();
    });

    createSheetSpy.mockRestore();
  });
});

/**
 * Devin Review finding 3315107576 (commit fb34260) flagged
 * `Draft.tsx`'s `onImportDxf` / `onExportDxf` for missing the
 * `getActiveProjectPath` defense-in-depth guard pattern that
 * `Bim.tsx onInvoke` / `Deliver.tsx onBuildPack` / `Render.tsx
 * enqueueAll` all use. The race is observable when the file dialog
 * is held open across a project switch: the picked DXF path was
 * chosen under project A's mental model, but the
 * `aec.draft.importDxf` IPC handler (in `electron/ipc.ts`) resolves
 * the project via `withResolvedProjectPath` at *dispatch* time —
 * so after the switch, the bridge call would attach project A's
 * DXF entities to project B's session.
 *
 * Real fix landed by capturing `startPath = getActiveProjectPath()`
 * at handler entry and re-checking after each dialog + bridge
 * await. On mismatch the handler aborts before dispatching the
 * bridge call and skips the success toast. These tests pin the
 * structural guarantee: a project switch that lands while the
 * dialog promise is pending must prevent the bridge call AND the
 * announcement toast.
 */
function DraftToolbarHarness({
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
    <>
      <button
        type="button"
        data-testid="switch-to-B"
        onClick={() => {
          void openProject(pathB);
        }}
      >
        switch
      </button>
      <Draft />
    </>
  );
}

describe("Draft page — DXF handlers guard against project-switch race", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("aborts onImportDxf when the file dialog is held open across a project switch", async () => {
    const PATH_A = "/tmp/draft-import-A.aecstudio";
    const PATH_B = "/tmp/draft-import-B.aecstudio";

    // Hold the open-file dialog promise so the test can switch
    // projects between dialog dispatch and dialog resolution — the
    // race the guard exists to close.
    let resolveDialog: (value: {
      canceled: boolean;
      paths: string[];
    }) => void = () => {
      /* assigned in mockImplementation */
    };
    const dialogPromise = new Promise<{
      canceled: boolean;
      paths: string[];
    }>((res) => {
      resolveDialog = res;
    });
    const openFileSpy = vi
      .spyOn(aec.dialog, "openFile")
      .mockReturnValue(dialogPromise);
    const importDxfSpy = vi
      .spyOn(aec.draft, "importDxf")
      .mockResolvedValue({ imported: 42 });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <DraftToolbarHarness pathA={PATH_A} pathB={PATH_B} />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Wait for project A to settle, then dispatch the import.
    await waitFor(() => {
      expect(screen.getByTestId("draft-import-dxf")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("draft-import-dxf"));
    });

    // Switch to project B *while* the file dialog is still pending.
    // This is the only window the guard protects — `RequireProject`
    // is not wrapped in this harness, so the page stays mounted.
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
      await Promise.resolve();
      await Promise.resolve();
    });

    // Now resolve the dialog with a picked path. Without the guard,
    // the handler would call `aec.draft.importDxf` with project A's
    // path against project B's active state.
    await act(async () => {
      resolveDialog({ canceled: false, paths: ["/tmp/some.dxf"] });
      await Promise.resolve();
      await Promise.resolve();
    });

    // The bridge call MUST NOT have been dispatched.
    expect(importDxfSpy).not.toHaveBeenCalled();
    // And no success toast was announced for project A on project B's UI.
    expect(screen.queryByText(/Imported 42 entities/)).toBeNull();

    openFileSpy.mockRestore();
    importDxfSpy.mockRestore();
  });

  it("aborts onExportDxf when the save dialog is held open across a project switch", async () => {
    const PATH_A = "/tmp/draft-export-A.aecstudio";
    const PATH_B = "/tmp/draft-export-B.aecstudio";

    let resolveSave: (value: { canceled: boolean; path: string }) => void = () => {
      /* assigned in mockImplementation */
    };
    const savePromise = new Promise<{
      canceled: boolean;
      path: string;
    }>((res) => {
      resolveSave = res;
    });
    const saveFileSpy = vi
      .spyOn(aec.dialog, "saveFile")
      .mockReturnValue(savePromise);
    const exportDxfSpy = vi
      .spyOn(aec.draft, "exportDxf")
      .mockResolvedValue({ exported: true, path: "/tmp/output.dxf" });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <DraftToolbarHarness pathA={PATH_A} pathB={PATH_B} />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    await waitFor(() => {
      expect(screen.getByTestId("draft-export-dxf")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("draft-export-dxf"));
    });

    // Switch projects mid-save-dialog.
    await act(async () => {
      fireEvent.click(screen.getByTestId("switch-to-B"));
      await Promise.resolve();
      await Promise.resolve();
    });

    await act(async () => {
      resolveSave({ canceled: false, path: "/tmp/output.dxf" });
      await Promise.resolve();
      await Promise.resolve();
    });

    // Bridge export must not fire against project B with project A's
    // intent. The success toast must also be suppressed.
    expect(exportDxfSpy).not.toHaveBeenCalled();
    expect(screen.queryByText(/Exported DXF to/)).toBeNull();

    saveFileSpy.mockRestore();
    exportDxfSpy.mockRestore();
  });

  it("dispatches onImportDxf normally when no project switch occurs", async () => {
    const PATH_A = "/tmp/draft-import-no-switch.aecstudio";

    const openFileSpy = vi
      .spyOn(aec.dialog, "openFile")
      .mockResolvedValue({ canceled: false, paths: ["/tmp/x.dxf"] });
    const importDxfSpy = vi
      .spyOn(aec.draft, "importDxf")
      .mockResolvedValue({ imported: 7 });

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <DraftToolbarHarness pathA={PATH_A} pathB={PATH_A} />
          <ToastContainer />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    await waitFor(() => {
      expect(screen.getByTestId("draft-import-dxf")).toBeInTheDocument();
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("draft-import-dxf"));
      await Promise.resolve();
      await Promise.resolve();
    });

    // Positive control: when no project switch lands, the bridge
    // call IS dispatched and the success toast appears. This pins
    // that the guard does not break the normal happy-path.
    expect(importDxfSpy).toHaveBeenCalledWith({ dxfPath: "/tmp/x.dxf" });
    await waitFor(() => {
      expect(screen.queryByText(/Imported 7 entities/)).not.toBeNull();
    });

    openFileSpy.mockRestore();
    importDxfSpy.mockRestore();
  });
});
