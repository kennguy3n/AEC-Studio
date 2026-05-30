import { Icon, type IconName } from "../../icons/Icon";

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

const TOOLS: { id: DesignTool; label: string; icon: IconName }[] = [
  { id: "select", label: "Select", icon: "select" },
  { id: "wall", label: "Wall", icon: "wall" },
  { id: "floor", label: "Floor", icon: "floor" },
  { id: "ceiling", label: "Ceiling", icon: "ceiling" },
  { id: "door", label: "Door", icon: "door" },
  { id: "window", label: "Window", icon: "window" },
  { id: "furniture", label: "Furniture", icon: "furniture" },
  { id: "material", label: "Material", icon: "material" },
  { id: "lighting", label: "Lighting", icon: "lighting" },
  { id: "camera", label: "Camera", icon: "camera" },
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
          <Icon name={t.icon} size={20} />
        </button>
      ))}
    </aside>
  );
}

export const DESIGN_TOOLS = TOOLS;
