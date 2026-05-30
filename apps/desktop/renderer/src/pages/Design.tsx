import { useRef, useState } from "react";
import { DesignToolbar, DesignTool } from "../components/design/DesignToolbar";
import { DesignInspector } from "../components/design/DesignInspector";
import { DesignAiPanel } from "../components/design/DesignAiPanel";
import { ViewportContainer } from "../components/design/ViewportContainer";
import {
  PanelResizeHandle,
  usePersistentPanelSize,
} from "../components/PanelResizeHandle";
import { useActiveProject } from "../hooks/useActiveProject";

// Phase 17 Group B Task 14 — persisted right-panel width for the
// Design mode. Min 200 px keeps the inspector readable; max 600 px
// keeps the viewport usable for picking on common laptop resolutions
// (≥ 1280 px wide).
const DESIGN_PANEL_MIN = 200;
const DESIGN_PANEL_MAX = 600;
const DESIGN_PANEL_DEFAULT = 320;

export function Design() {
  // The active project is consumed here to ensure the route guard has
  // checked that a project is open. The design commands themselves
  // (placeFurniture, paintMaterial, etc.) resolve the project path on
  // the main-process side via `withResolvedProjectPath` in ipc.ts, so
  // there's no need to thread `project.path` into every child — the
  // children call `aec.design.*` without an explicit projectPath and
  // the IPC layer injects it from the active-project tracker.
  useActiveProject();
  const [activeTool, setActiveTool] = useState<DesignTool>("select");
  const [panelWidth, setPanelWidth] = usePersistentPanelSize(
    "panel.design.inspector",
    DESIGN_PANEL_DEFAULT,
    { min: DESIGN_PANEL_MIN, max: DESIGN_PANEL_MAX },
  );
  // The handle reports a delta in screen pixels relative to the
  // drag start; capture the size at pointerdown so the running
  // computation is a pure add.
  const dragStartWidth = useRef<number>(panelWidth);
  return (
    <div
      className="design-layout"
      data-testid="design-mode"
      style={{
        gridTemplateColumns: `64px 1fr auto ${panelWidth}px`,
      }}
    >
      <DesignToolbar activeTool={activeTool} onSelect={setActiveTool} />
      <ViewportContainer activeTool={activeTool} />
      <PanelResizeHandle
        orientation="vertical"
        label="Resize inspector panel"
        data-testid="design-panel-resize"
        onResizeStart={() => {
          dragStartWidth.current = panelWidth;
        }}
        onResize={(delta) => {
          // Dragging right grows the *viewport* (left side) — the
          // inspector is on the right of the handle, so its width
          // shrinks as delta grows. Mirror the sign accordingly.
          setPanelWidth(dragStartWidth.current - delta);
        }}
      />
      <div className="design-panels">
        <DesignInspector activeTool={activeTool} />
        <DesignAiPanel />
      </div>
    </div>
  );
}
