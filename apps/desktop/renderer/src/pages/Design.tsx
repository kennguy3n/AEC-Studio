import { useState } from "react";
import { DesignToolbar, DesignTool } from "../components/design/DesignToolbar";
import { DesignInspector } from "../components/design/DesignInspector";
import { DesignAiPanel } from "../components/design/DesignAiPanel";
import { ViewportContainer } from "../components/design/ViewportContainer";
import { useActiveProject } from "../hooks/useActiveProject";

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
  return (
    <div className="design-layout" data-testid="design-mode">
      <DesignToolbar activeTool={activeTool} onSelect={setActiveTool} />
      <ViewportContainer activeTool={activeTool} />
      <div className="design-panels">
        <DesignInspector activeTool={activeTool} />
        <DesignAiPanel />
      </div>
    </div>
  );
}
