import { afterEach, describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { Draft } from "../pages/Draft";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";
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
