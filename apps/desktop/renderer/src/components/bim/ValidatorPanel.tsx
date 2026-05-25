import { useState } from "react";
import { aec } from "../../api/aec";
import {
  bimReportToFindings,
  type ValidationFinding,
  type ValidationSeverity,
} from "../../api/bim-validation";

// Re-exported for backwards compat with existing callers (tests,
// `Bim.tsx`) that previously imported these types from the
// component module. The canonical definitions now live in
// `api/bim-validation.ts` alongside the wire-format adapter.
export type { ValidationFinding, ValidationSeverity };

interface Props {
  /** IFC path to validate when "Re-validate" is clicked. */
  sourcePath: string;
  findings: ValidationFinding[];
  onFindings: (next: ValidationFinding[]) => void;
  onZoomTo?: (entityId: string) => void;
}

export function ValidatorPanel({
  sourcePath,
  findings,
  onFindings,
  onZoomTo,
}: Props) {
  const [busy, setBusy] = useState(false);

  const revalidate = async () => {
    setBusy(true);
    try {
      const result = await aec.bim.validate({ sourcePath });
      onFindings(bimReportToFindings(result));
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
