import { useState } from "react";
import { DesignToolbar, DesignTool } from "../components/design/DesignToolbar";
import { DesignInspector } from "../components/design/DesignInspector";
import { DesignAiPanel } from "../components/design/DesignAiPanel";
import { ViewportContainer } from "../components/design/ViewportContainer";

export function Design() {
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
