export type DesignTool =
  | "select"
  | "wall"
  | "floor"
  | "ceiling"
  | "door"
  | "window"
  | "furniture"
  | "material"
  | "lighting"
  | "camera";

const TOOLS: { id: DesignTool; label: string; icon: string }[] = [
  { id: "select", label: "Select", icon: "↑" },
  { id: "wall", label: "Wall", icon: "║" },
  { id: "floor", label: "Floor", icon: "▤" },
  { id: "ceiling", label: "Ceiling", icon: "▥" },
  { id: "door", label: "Door", icon: "◰" },
  { id: "window", label: "Window", icon: "▦" },
  { id: "furniture", label: "Furniture", icon: "▢" },
  { id: "material", label: "Material", icon: "◔" },
  { id: "lighting", label: "Lighting", icon: "☀" },
  { id: "camera", label: "Camera", icon: "◉" },
];

interface Props {
  activeTool: DesignTool;
  onSelect: (tool: DesignTool) => void;
}

export function DesignToolbar({ activeTool, onSelect }: Props) {
  return (
    <aside className="design-toolbar" role="toolbar" aria-label="Design tools">
      {TOOLS.map((t) => (
        <button
          key={t.id}
          type="button"
          title={t.label}
          aria-label={t.label}
          aria-pressed={activeTool === t.id}
          data-testid={`tool-${t.id}`}
          className={`design-toolbar__btn${activeTool === t.id ? " is-active" : ""}`}
          onClick={() => onSelect(t.id)}
        >
          {t.icon}
        </button>
      ))}
    </aside>
  );
}

export const DESIGN_TOOLS = TOOLS;
