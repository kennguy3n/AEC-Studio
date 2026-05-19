import { useEffect, useState } from "react";
import { aec, RuntimeStatus } from "../api/aec";

export function StatusBar() {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [aiState, setAiState] = useState<string>("idle");

  useEffect(() => {
    void aec.runtime.status().then((s) => setStatus(s as RuntimeStatus));
    void aec.ai.runtimeStatus().then((s) => setAiState((s as { state: string }).state));
  }, []);

  return (
    <footer className="status-bar" role="status" aria-live="polite">
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
      <span style={{ marginLeft: "auto" }}>AI · {aiState}</span>
    </footer>
  );
}

function fmtGb(mb: number): string {
  return `${(mb / 1024).toFixed(1)} GB`;
}
