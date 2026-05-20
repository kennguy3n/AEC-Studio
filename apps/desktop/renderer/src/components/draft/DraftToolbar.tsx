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

export const DRAW_TOOLS: { id: DraftDrawTool; label: string }[] = [
  { id: "line", label: "Line" },
  { id: "polyline", label: "Polyline" },
  { id: "arc", label: "Arc" },
  { id: "circle", label: "Circle" },
  { id: "text", label: "Text" },
  { id: "dim", label: "Dim" },
  { id: "hatch", label: "Hatch" },
];

export const EDIT_TOOLS: { id: DraftEditTool; label: string }[] = [
  { id: "move", label: "Move" },
  { id: "copy", label: "Copy" },
  { id: "rotate", label: "Rotate" },
  { id: "trim", label: "Trim" },
  { id: "extend", label: "Extend" },
  { id: "offset", label: "Offset" },
  { id: "fillet", label: "Fillet" },
  { id: "chamfer", label: "Chamfer" },
  { id: "mirror", label: "Mirror" },
  { id: "scale", label: "Scale" },
];

interface Props {
  activeTool: DraftTool;
  onSelect: (tool: DraftTool) => void;
}

export function DraftToolbar({ activeTool, onSelect }: Props) {
  return (
    <aside className="draft-toolbar" role="toolbar" aria-label="Draft tools">
      <button
        type="button"
        className={`draft-toolbar__btn${activeTool === "select" ? " draft-toolbar__btn--active" : ""}`}
        onClick={() => onSelect("select")}
        data-testid="draft-tool-select"
      >
        Select
      </button>
      <div className="draft-toolbar__group" data-testid="draft-toolbar-draw">
        {DRAW_TOOLS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`draft-toolbar__btn${activeTool === t.id ? " draft-toolbar__btn--active" : ""}`}
            onClick={() => onSelect(t.id)}
            data-testid={`draft-tool-${t.id}`}
          >
            {t.label}
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
          >
            {t.label}
          </button>
        ))}
      </div>
    </aside>
  );
}
