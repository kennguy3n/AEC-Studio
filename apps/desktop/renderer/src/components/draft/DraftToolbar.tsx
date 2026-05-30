import { Icon, type IconName } from "../../icons/Icon";

export type DraftDrawTool =
  | "line"
  | "polyline"
  | "circle"
  | "arc"
  | "text"
  | "dim"
  | "hatch";

export type DraftEditTool =
  | "move"
  | "copy"
  | "rotate"
  | "trim"
  | "extend"
  | "offset"
  | "fillet"
  | "chamfer"
  | "mirror"
  | "scale";

export type DraftTool = DraftDrawTool | DraftEditTool | "select";

export const DRAW_TOOLS: { id: DraftDrawTool; label: string; icon: IconName }[] = [
  { id: "line", label: "Line", icon: "line" },
  { id: "polyline", label: "Polyline", icon: "polyline" },
  { id: "arc", label: "Arc", icon: "arc" },
  { id: "circle", label: "Circle", icon: "circle" },
  { id: "text", label: "Text", icon: "text" },
  { id: "dim", label: "Dim", icon: "dim" },
  { id: "hatch", label: "Hatch", icon: "hatch" },
];

export const EDIT_TOOLS: { id: DraftEditTool; label: string; icon: IconName }[] = [
  { id: "move", label: "Move", icon: "move" },
  { id: "copy", label: "Copy", icon: "copy" },
  { id: "rotate", label: "Rotate", icon: "rotate" },
  { id: "trim", label: "Trim", icon: "trim" },
  { id: "extend", label: "Extend", icon: "extend" },
  { id: "offset", label: "Offset", icon: "offset" },
  { id: "fillet", label: "Fillet", icon: "fillet" },
  { id: "chamfer", label: "Chamfer", icon: "chamfer" },
  { id: "mirror", label: "Mirror", icon: "mirror" },
  { id: "scale", label: "Scale", icon: "scale" },
];

interface Props {
  activeTool: DraftTool;
  onSelect: (tool: DraftTool) => void;
  onImport?: () => void;
  onExport?: () => void;
}

export function DraftToolbar({ activeTool, onSelect, onImport, onExport }: Props) {
  return (
    <aside className="draft-toolbar" role="toolbar" aria-label="Draft tools">
      <button
        type="button"
        className={`draft-toolbar__btn${activeTool === "select" ? " draft-toolbar__btn--active" : ""}`}
        onClick={() => onSelect("select")}
        data-testid="draft-tool-select"
        title="Select"
      >
        <Icon name="select" size={18} />
        <span className="draft-toolbar__label">Select</span>
      </button>
      <div className="draft-toolbar__group" data-testid="draft-toolbar-draw">
        {DRAW_TOOLS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`draft-toolbar__btn${activeTool === t.id ? " draft-toolbar__btn--active" : ""}`}
            onClick={() => onSelect(t.id)}
            data-testid={`draft-tool-${t.id}`}
            title={t.label}
          >
            <Icon name={t.icon} size={18} />
            <span className="draft-toolbar__label">{t.label}</span>
          </button>
        ))}
      </div>
      <div className="draft-toolbar__group" data-testid="draft-toolbar-edit">
        {EDIT_TOOLS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`draft-toolbar__btn${activeTool === t.id ? " draft-toolbar__btn--active" : ""}`}
            onClick={() => onSelect(t.id)}
            data-testid={`draft-tool-${t.id}`}
            title={t.label}
          >
            <Icon name={t.icon} size={18} />
            <span className="draft-toolbar__label">{t.label}</span>
          </button>
        ))}
      </div>
      {(onImport || onExport) && (
        <div className="draft-toolbar__group" data-testid="draft-toolbar-io">
          {onImport && (
            <button
              type="button"
              className="draft-toolbar__btn"
              onClick={onImport}
              data-testid="draft-import-dxf"
            >
              Import
            </button>
          )}
          {onExport && (
            <button
              type="button"
              className="draft-toolbar__btn"
              onClick={onExport}
              data-testid="draft-export-dxf"
            >
              Export
            </button>
          )}
        </div>
      )}
    </aside>
  );
}
