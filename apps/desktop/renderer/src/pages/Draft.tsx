import { useState } from "react";
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

export function Draft() {
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

  return (
    <div className="draft-layout" data-testid="draft-mode">
      <DraftToolbar activeTool={activeTool} onSelect={setActiveTool} />
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
