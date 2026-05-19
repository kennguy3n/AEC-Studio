import { useEffect, useState } from "react";
import { aec, AiTool } from "../../api/aec";

export function DesignAiPanel() {
  const [tools, setTools] = useState<AiTool[]>([]);

  useEffect(() => {
    let alive = true;
    void aec.ai.listTools().then((t) => {
      if (alive) setTools(t as AiTool[]);
    });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <section className="design-panel" aria-label="AI assistant">
      <div className="design-panel__title">PrismML</div>
      <p style={{ fontSize: 12, color: "var(--aec-color-text-secondary)" }}>
        Local AI. All tool calls run on this device.
      </p>
      <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
        {tools
          .filter((t) => t.scope.includes("design"))
          .map((t) => (
            <li key={t.id} style={{ marginBottom: 8 }}>
              <button
                type="button"
                className="button button--ghost"
                style={{ width: "100%", justifyContent: "flex-start" }}
                data-testid={`ai-tool-${t.id}`}
              >
                {t.id.replace(/_/g, " ")}
              </button>
            </li>
          ))}
      </ul>
    </section>
  );
}
