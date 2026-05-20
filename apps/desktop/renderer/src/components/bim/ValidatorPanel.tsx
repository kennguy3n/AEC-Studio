import { useState } from "react";
import { aec } from "../../api/aec";

export type ValidationSeverity = "error" | "warning" | "info";

export interface ValidationFinding {
  code: string;
  severity: ValidationSeverity;
  message: string;
  entityId?: string | null;
  hint?: string | null;
}

interface Props {
  findings: ValidationFinding[];
  onFindings: (next: ValidationFinding[]) => void;
  onZoomTo?: (entityId: string) => void;
}

export function ValidatorPanel({ findings, onFindings, onZoomTo }: Props) {
  const [busy, setBusy] = useState(false);

  const revalidate = async () => {
    setBusy(true);
    try {
      const result = (await aec.bim.validate()) as {
        ok: boolean;
        errors: ValidationFinding[];
        warnings: ValidationFinding[];
        info?: ValidationFinding[];
      };
      const merged: ValidationFinding[] = [
        ...(result.errors ?? []).map((f) => ({ ...f, severity: "error" as const })),
        ...(result.warnings ?? []).map((f) => ({ ...f, severity: "warning" as const })),
        ...(result.info ?? []).map((f) => ({ ...f, severity: "info" as const })),
      ];
      onFindings(merged);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className="bim-validator"
      aria-label="Validation findings"
      data-testid="validator-panel"
    >
      <header>
        <span data-testid="validator-count">{findings.length} finding(s)</span>
        <button
          type="button"
          data-testid="validator-revalidate"
          onClick={revalidate}
          disabled={busy}
        >
          {busy ? "Validating…" : "Re-validate"}
        </button>
      </header>
      <ul className="bim-validator__list">
        {findings.length === 0 && (
          <li
            className="bim-validator__empty"
            data-testid="validator-empty"
          >
            No findings.
          </li>
        )}
        {findings.map((f, i) => (
          <li
            key={i}
            className={`bim-validator__item bim-validator__item--${f.severity}`}
            data-testid={`validator-item-${i}`}
          >
            <span className="bim-validator__sev">
              {f.severity.toUpperCase()}
            </span>
            <span className="bim-validator__code">{f.code}</span>
            <span className="bim-validator__msg">{f.message}</span>
            {f.entityId && onZoomTo && (
              <button
                type="button"
                className="bim-validator__zoom"
                data-testid={`validator-zoom-${i}`}
                onClick={() => onZoomTo(f.entityId!)}
              >
                Zoom
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
