import { useEffect, useState } from "react";
import { aec, RuntimeStatus } from "../api/aec";
import { KChatStatusIndicator } from "./kchat/KChatStatusIndicator";
import { useActiveProject } from "../hooks/useActiveProject";

export function StatusBar() {
  const { project, dirty, undoLen, redoLen, saving } = useActiveProject();
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [aiState, setAiState] = useState<string>("idle");
  const [renderJobCount, setRenderJobCount] = useState<number>(0);

  useEffect(() => {
    let alive = true;
    void aec.runtime.status().then((s) => {
      if (alive) setStatus(s as RuntimeStatus);
    });
    void aec.ai.runtimeStatus().then((s) => {
      if (alive) setAiState((s as { state: string }).state);
    });
    // Poll render queue count every 5s so the StatusBar reflects
    // active jobs without subscribing to per-job events.
    const tick = () => {
      void aec.render.listJobs().then((jobs) => {
        if (!alive) return;
        const active = (jobs as Array<{ status: string }>).filter(
          (j) => j.status === "queued" || j.status === "running",
        ).length;
        setRenderJobCount(active);
      });
    };
    tick();
    const id = window.setInterval(tick, 5000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, []);

  return (
    <footer className="status-bar" role="status" aria-live="polite">
      {project && (
        <span className="status-bar__chip status-bar__chip--project">
          <strong>{project.name}</strong>
          {saving ? (
            <span className="status-bar__save" data-testid="status-saving">
              · Saving…
            </span>
          ) : dirty ? (
            <span
              className="status-bar__save status-bar__save--dirty"
              data-testid="status-dirty"
            >
              · Unsaved
            </span>
          ) : (
            <span className="status-bar__save" data-testid="status-saved">
              · Saved
            </span>
          )}
        </span>
      )}
      {project && (
        <span
          className="status-bar__undo-redo"
          data-testid="status-undo-redo"
          title={`Undo: ${undoLen} · Redo: ${redoLen}`}
        >
          ↶{undoLen} ↷{redoLen}
        </span>
      )}
      {status ? (
        <>
          <span className={`status-bar__chip is-tier-${status.tier}`}>
            <strong>{status.tier}</strong> tier
          </span>
          <span>
            CPU {status.cpu.physicalCores}c/{status.cpu.logicalCores}t
          </span>
          <span>
            RAM {fmtGb(status.ramAvailableMb)} free / {fmtGb(status.ramTotalMb)}
          </span>
          {status.gpu ? (
            <span>
              GPU {status.gpu.vendor} {status.gpu.model}
            </span>
          ) : (
            <span>GPU not detected</span>
          )}
        </>
      ) : (
        <span>Loading hardware profile…</span>
      )}
      {renderJobCount > 0 && (
        <span
          className="status-bar__render-count"
          data-testid="status-render-count"
        >
          ⏵ {renderJobCount} render{renderJobCount === 1 ? "" : "s"}
        </span>
      )}
      <span style={{ marginLeft: "auto" }}>AI · {aiState}</span>
      <KChatStatusIndicator />
    </footer>
  );
}

function fmtGb(mb: number): string {
  return `${(mb / 1024).toFixed(1)} GB`;
}
