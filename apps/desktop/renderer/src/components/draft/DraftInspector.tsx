import type { DraftTool } from "./DraftToolbar";

export interface DraftSelection {
  count: number;
  primaryType: string | null;
  layer: string | null;
}

interface Props {
  selection: DraftSelection;
  activeTool: DraftTool;
}

const DIM_PRECISION_OPTIONS = [0, 1, 2, 3, 4] as const;
const TEXT_HEIGHT_OPTIONS = [2.5, 3.5, 5, 7, 10] as const;

export function DraftInspector({ selection, activeTool }: Props) {
  return (
    <section className="draft-panel" aria-label="Inspector" data-testid="draft-inspector">
      <div className="draft-panel__title">Properties</div>
      <div className="draft-panel__row">
        <span>Active tool</span>
        <span data-testid="inspector-tool">{activeTool}</span>
      </div>
      <div className="draft-panel__row">
        <span>Selection</span>
        <span data-testid="inspector-count">
          {selection.count === 0 ? "None" : `${selection.count} entit${selection.count > 1 ? "ies" : "y"}`}
        </span>
      </div>
      <div className="draft-panel__row">
        <span>Layer</span>
        <span>{selection.layer ?? "—"}</span>
      </div>
      <div className="draft-panel__row">
        <span>Type</span>
        <span>{selection.primaryType ?? "—"}</span>
      </div>
      {activeTool === "dim" ? (
        <fieldset className="draft-panel__fieldset" data-testid="inspector-dim-style">
          <legend>Dim style</legend>
          <label>
            Precision
            <select defaultValue={2}>
              {DIM_PRECISION_OPTIONS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>
          <label>
            Text height (mm)
            <select defaultValue={2.5}>
              {TEXT_HEIGHT_OPTIONS.map((h) => (
                <option key={h} value={h}>
                  {h}
                </option>
              ))}
            </select>
          </label>
        </fieldset>
      ) : null}
    </section>
  );
}
