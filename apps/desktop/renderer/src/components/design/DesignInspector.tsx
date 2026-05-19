import type { DesignTool } from "./DesignToolbar";
import { MaterialPanel } from "./MaterialPanel";
import { AssetBrowser } from "../AssetBrowser";

interface Props {
  activeTool: DesignTool;
}

export function DesignInspector({ activeTool }: Props) {
  return (
    <section className="design-panel" aria-label="Inspector">
      <div className="design-panel__title">Inspector</div>
      {activeTool === "material" ? (
        <MaterialPanel />
      ) : activeTool === "furniture" ? (
        <AssetBrowser />
      ) : (
        <div className="material-inspector__row">
          <span>Selection</span>
          <span>None</span>
        </div>
      )}
    </section>
  );
}
