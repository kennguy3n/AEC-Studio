import type { RuntimeStatus } from "../api/aec";

interface Props {
  status: RuntimeStatus | null;
}

export function HardwareProfileCard({ status }: Props) {
  return (
    <section className="card hw-card" aria-label="Hardware profile">
      <div className="hw-card__metric">
        <span className="hw-card__label">Tier</span>
        <span className="hw-card__value">{status?.tier ?? "—"}</span>
      </div>
      <div className="hw-card__metric">
        <span className="hw-card__label">CPU</span>
        <span className="hw-card__value">
          {status
            ? `${status.cpu.physicalCores}c / ${status.cpu.logicalCores}t`
            : "—"}
        </span>
      </div>
      <div className="hw-card__metric">
        <span className="hw-card__label">RAM</span>
        <span className="hw-card__value">
          {status ? `${(status.ramTotalMb / 1024).toFixed(1)} GB` : "—"}
        </span>
      </div>
      <div className="hw-card__metric">
        <span className="hw-card__label">GPU</span>
        <span className="hw-card__value">
          {status?.gpu ? `${status.gpu.vendor} ${status.gpu.model}` : "—"}
        </span>
      </div>
    </section>
  );
}
