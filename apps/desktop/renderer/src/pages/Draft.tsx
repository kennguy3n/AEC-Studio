import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { aec } from "../api/aec";
import { projectGraphList } from "../api/commands";
import { DraftToolbar, DraftTool } from "../components/draft/DraftToolbar";
import {
  extractPrimitives,
  type Primitive,
} from "../components/draft/paint-primitives";
import {
  PanelResizeHandle,
  usePersistentPanelSize,
} from "../components/PanelResizeHandle";
import {
  LayerPanel,
  DEFAULT_LAYERS,
  LayerState,
} from "../components/draft/LayerPanel";
import { SheetManager, SheetTab } from "../components/draft/SheetManager";
import {
  CommandLine,
  CommandLineLogEntry,
} from "../components/draft/CommandLine";
import {
  DraftInspector,
  DraftSelection,
} from "../components/draft/DraftInspector";
import { DraftCanvas } from "../components/draft/DraftCanvas";
import { useActiveProject } from "../hooks/useActiveProject";
import { useToast } from "../hooks/useToast";

const DXF_FILTERS = [
  { name: "DXF / DWG Files", extensions: ["dxf", "dwg"] },
];

export function Draft() {
  const { project, getActiveProjectPath } = useActiveProject();
  const { addToast } = useToast();
  const [activeTool, setActiveTool] = useState<DraftTool>("select");
  const [layers, setLayers] = useState<LayerState[]>(DEFAULT_LAYERS);
  const [sheets, setSheets] = useState<SheetTab[]>([
    { id: "sheet-default", name: "Sheet 1" },
  ]);
  const [activeSheet, setActiveSheet] = useState<string | null>(
    "sheet-default",
  );
  const [log, setLog] = useState<CommandLineLogEntry[]>([]);
  const [selection] = useState<DraftSelection>({
    count: 0,
    primaryType: null,
    layer: null,
  });
  // Phase 17 Group B Task 14 — persisted right-panel width.
  const [panelWidth, setPanelWidth, commitPanelWidth] = usePersistentPanelSize(
    "panel.draft.right",
    320,
    { min: 200, max: 600 },
  );
  const dragStartWidth = useRef<number>(panelWidth);

  // Phase 17 Group C Task 20 — imported DXF primitives hydrated
  // from the project graph (`projectGraphList(path, "primitive")`).
  // Reset to `[]` on every project transition, refetched on mount
  // and after every successful DXF import below.
  const [primitives, setPrimitives] = useState<Primitive[]>([]);

  const refreshPrimitives = useCallback(
    async (targetPath: string | null) => {
      if (!targetPath) {
        setPrimitives([]);
        return;
      }
      try {
        const rows = await projectGraphList(targetPath, "primitive");
        // Defense-in-depth guard: if a project transition raced the
        // await above, drop the result rather than land project A's
        // primitives onto project B's canvas. The fetch did execute
        // against project A and the rows ARE valid for project A's
        // session — we just decline to display them under project
        // B's UI. Project B's own mount effect will fire its own
        // fetch with project B's path.
        if (getActiveProjectPath() !== targetPath) return;
        setPrimitives(extractPrimitives(rows));
      } catch {
        // Bridge error — keep whatever was already on the canvas
        // (or the empty state for a fresh project). The user can
        // re-trigger by reopening the project. Surfaces no toast
        // because this fetch is a background hydration step, not a
        // user-initiated gesture.
      }
    },
    [getActiveProjectPath],
  );

  // Refresh on project mount/change.
  useEffect(() => {
    void refreshPrimitives(project?.path ?? null);
  }, [project?.path, refreshPrimitives]);

  // Reset per-project state on project transitions. Today this fires
  // only on initial mount because `RequireProject` unmounts the Draft
  // page on every project switch (so `useState` resets naturally) —
  // but the unmount-on-switch invariant is a route-guard convention,
  // not a contract the page itself owns. Any future in-page project
  // picker, a `Recent project` jump from a side panel, or any flow
  // that calls `openProject` without navigating away from `/draft`
  // would silently leak project A's layers / sheets / command log /
  // selection into project B. Mirroring the explicit reset that
  // `Bim.tsx:94-110` and `Render.tsx:90-149` already use makes the
  // contract structural — the page owns its own per-project reset
  // regardless of how the parent route handles transitions. The
  // reset is synchronous (no IPC), so there is no microtask window
  // for stale state to render in.
  useEffect(() => {
    setActiveTool("select");
    setLayers(DEFAULT_LAYERS);
    setSheets([{ id: "sheet-default", name: "Sheet 1" }]);
    setActiveSheet("sheet-default");
    setLog([]);
    setPrimitives([]);
  }, [project?.path]);

  // Phase 17 Group C Task 20 — the visible-layer set + per-layer
  // color map derived from the user's LayerPanel state. Memoised
  // so the `DraftCanvas` repaint effect only re-fires when the
  // panel state actually changes (not on every keystroke in some
  // unrelated input). Both maps are passed by-reference into the
  // canvas; the empty-state where every layer is visible (the
  // initial `DEFAULT_LAYERS` set) reduces to a no-op filter inside
  // `paintPrimitives`.
  const visibleLayers = useMemo(
    () => new Set(layers.filter((l) => l.visible).map((l) => l.name)),
    [layers],
  );
  const layerColors = useMemo(() => {
    const m = new Map<string, string>();
    for (const l of layers) m.set(l.name, l.color);
    return m;
  }, [layers]);

  // Defense-in-depth guard pattern matching `Bim.tsx`,
  // `Deliver.tsx`, and `Render.tsx`. The DXF import/export handlers
  // await two cross-process round-trips each (open/save dialog,
  // then bridge importDxf/exportDxf), so a project switch can land
  // between them. If the user opens project B during the file
  // dialog, the picked path was chosen under project A's mental
  // model; importing it would attach project A's DXF entities to
  // project B's bridge state via `withResolvedProjectPath` in the
  // IPC layer. Similarly, the success toast for an export started
  // under project A would announce against project B's UI — the
  // toast-text analogue of the same race closed for state commits
  // in the other pages.
  //
  //   * Today the `RequireProject` route guard unmounts Draft on
  //     every project transition, so a stale `addToast` would be a
  //     no-op on a torn-down component and the bridge calls would
  //     not fire post-unmount. The page is safe in production.
  //   * BUT the route-guard umbrella is the same brittle contract
  //     that motivated the per-project reset effect above to NOT
  //     rely on it. Future in-page project pickers or any code
  //     path that calls `openProject(...)` without forcing a
  //     route change would let project A's import/export land
  //     against project B's session.
  //
  // Devin Review flagged the absence of this guard as a defense-
  // in-depth inconsistency: `Bim.tsx onInvoke` / `Deliver.tsx
  // onBuildPack` both check `getActiveProjectPath()` after every
  // dialog and bridge await; `Draft.tsx` did not. Closing the
  // gap here keeps all five mode pages on one structural pattern
  // so a future contributor reading any one of them sees the
  // same guard shape.
  //
  // Routes through `getActiveProjectPath()` (the central sync
  // getter exposed by `useActiveProject`) rather than a local
  // `useRef + useEffect` mirror so the guard reads the freshest
  // sync value — see the `getActiveProjectPath` docblock on
  // `ActiveProjectState` for the full timing analysis.
  const onImportDxf = async () => {
    const startPath = getActiveProjectPath();
    const dialog = await aec.dialog.openFile({
      title: "Import DXF/DWG",
      filters: DXF_FILTERS,
    });
    if (dialog.canceled || dialog.paths.length === 0) return;
    // If the user project-switched while the file dialog was open,
    // the picked DXF path was chosen for project A but the active
    // project is now B. Aborting the import here avoids attaching
    // project A's DXF entities to project B's session; the user can
    // re-trigger import from project B's Draft page.
    if (getActiveProjectPath() !== startPath) return;
    const dxfPath = dialog.paths[0];
    try {
      const result = await aec.draft.importDxf({ dxfPath });
      // Skip stale: don't announce a project-A import on project B's
      // UI. The bridge call did land project A's entities into
      // project A's session (`withResolvedProjectPath` resolved at
      // dispatch time — see the `electron/ipc.ts` handler), so the
      // import is durable; this just declines the toast on the wrong
      // project's page.
      if (getActiveProjectPath() !== startPath) return;
      const imported =
        typeof result === "object" && result !== null && "imported" in result
          ? (result as { imported: number }).imported
          : 0;
      addToast("success", `Imported ${imported} entities from DXF`);
      // Refresh the canvas so the newly imported primitives are
      // painted immediately — without this the user has to switch
      // projects + back to see what they just imported.
      void refreshPrimitives(startPath);
    } catch (err) {
      // Errors are project-agnostic UX (matches the `Bim.tsx`
      // `onInvoke` and `Deliver.tsx onBuildPack` conventions): even
      // after a project switch, "your last import failed" is useful
      // for the user. Toast unconditionally.
      addToast(
        "error",
        `DXF import failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  const onExportDxf = async () => {
    const startPath = getActiveProjectPath();
    const defaultName = project ? `${project.name}.dxf` : "drawing.dxf";
    const dialog = await aec.dialog.saveFile({
      title: "Export DXF",
      defaultPath: defaultName,
      filters: DXF_FILTERS,
    });
    if (dialog.canceled || !dialog.path) return;
    // If the user project-switched while the save dialog was open,
    // the chosen output path was picked under project A's mental
    // model. Honoring the export would write project A's drawing
    // bytes against project B's bridge state (the bridge resolves
    // its source from the active path at dispatch time). Abort the
    // export entirely; the user can re-trigger from the new project.
    if (getActiveProjectPath() !== startPath) return;
    try {
      await aec.draft.exportDxf({ dxfPath: dialog.path });
      // Skip stale success toast: announcing a project-A export on
      // project B's UI would mislead the user. The file itself was
      // still written to the user-chosen path (the bridge
      // dispatched against project A); this just declines to
      // announce it on project B.
      if (getActiveProjectPath() !== startPath) return;
      addToast("success", `Exported DXF to ${dialog.path}`);
    } catch (err) {
      addToast(
        "error",
        `DXF export failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  return (
    <div
      className="draft-layout"
      data-testid="draft-mode"
      style={{
        display: "grid",
        gridTemplateColumns: `auto 1fr auto ${panelWidth}px`,
        gap: "var(--aec-space-3)",
        height: "calc(100vh - var(--aec-status-bar-height))",
      }}
    >
      <DraftToolbar
        activeTool={activeTool}
        onSelect={setActiveTool}
        onImport={onImportDxf}
        onExport={onExportDxf}
      />
      <div className="draft-center">
        <SheetManager
          sheets={sheets}
          activeId={activeSheet}
          onChange={(s, id) => {
            setSheets(s);
            setActiveSheet(id);
          }}
        />
        <DraftCanvas
          activeTool={activeTool}
          primitives={primitives}
          visibleLayers={visibleLayers}
          layerColors={layerColors}
        />
        <CommandLine log={log} onLog={setLog} />
      </div>
      <PanelResizeHandle
        orientation="vertical"
        label="Resize draft side panel"
        data-testid="draft-panel-resize"
        onResizeStart={() => {
          dragStartWidth.current = panelWidth;
        }}
        onResize={(delta) => {
          setPanelWidth(dragStartWidth.current - delta);
        }}
        // Persist once per gesture rather than on every pointermove
        // — see `usePersistentPanelSize` docstring.
        onResizeEnd={commitPanelWidth}
      />
      <div className="draft-panels">
        <LayerPanel layers={layers} onChange={setLayers} />
        <DraftInspector selection={selection} activeTool={activeTool} />
      </div>
    </div>
  );
}
