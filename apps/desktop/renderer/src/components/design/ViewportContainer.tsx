import type { DesignTool } from "./DesignToolbar";

interface Props {
  activeTool: DesignTool;
}

export function ViewportContainer({ activeTool }: Props) {
  return (
    <section
      className="design-viewport"
      data-testid="design-viewport"
      data-active-tool={activeTool}
      aria-label="3D viewport"
    >
      <div>
        <div style={{ fontWeight: 600, fontSize: 13 }}>3D Viewport</div>
        <div style={{ fontSize: 12, color: "var(--aec-color-text-muted)" }}>
          Tool: {activeTool}
        </div>
      </div>
    </section>
  );
}
