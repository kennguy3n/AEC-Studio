import { useState } from "react";
import { aec } from "../api/aec";
import { DraftToolbar, DraftTool } from "../components/draft/DraftToolbar";
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
  const { project } = useActiveProject();
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

  const onImportDxf = async () => {
    const dialog = await aec.dialog.openFile({
      title: "Import DXF/DWG",
      filters: DXF_FILTERS,
    });
    if (dialog.canceled || dialog.paths.length === 0) return;
    const dxfPath = dialog.paths[0];
    try {
      const result = await aec.draft.importDxf({ dxfPath });
      const imported =
        typeof result === "object" && result !== null && "imported" in result
          ? (result as { imported: number }).imported
          : 0;
      addToast("success", `Imported ${imported} entities from DXF`);
    } catch (err) {
      addToast(
        "error",
        `DXF import failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  const onExportDxf = async () => {
    const defaultName = project ? `${project.name}.dxf` : "drawing.dxf";
    const dialog = await aec.dialog.saveFile({
      title: "Export DXF",
      defaultPath: defaultName,
      filters: DXF_FILTERS,
    });
    if (dialog.canceled || !dialog.path) return;
    try {
      await aec.draft.exportDxf({ dxfPath: dialog.path });
      addToast("success", `Exported DXF to ${dialog.path}`);
    } catch (err) {
      addToast(
        "error",
        `DXF export failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  return (
    <div className="draft-layout" data-testid="draft-mode">
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
        <DraftCanvas activeTool={activeTool} />
        <CommandLine log={log} onLog={setLog} />
      </div>
      <div className="draft-panels">
        <LayerPanel layers={layers} onChange={setLayers} />
        <DraftInspector selection={selection} activeTool={activeTool} />
      </div>
    </div>
  );
}
