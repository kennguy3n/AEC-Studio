import { useState } from "react";
import { aec } from "../../api/aec";

export interface DoctorSuggestion {
  code: string;
  severity: "info" | "warning" | "error";
  message: string;
  fix?: string | null;
  /**
   * When the suggestion comes from the material check, this is the
   * material id it applies to. The UI groups material findings under
   * a "Materials" header.
   */
  materialId?: string | null;
}

interface Props {
  jobId: string | null;
  suggestions: DoctorSuggestion[];
  onSuggestions: (next: DoctorSuggestion[]) => void;
  /**
   * Called when the user clicks "Check Materials". The page wires
   * this to the bridge material-check IPC; suggestions returned are
   * merged via `onSuggestions` so the same row list shows both
   * render-time and material findings.
   */
  onCheckMaterials?: () => Promise<DoctorSuggestion[] | undefined> | void;
}

export function RenderDoctor({
  jobId,
  suggestions,
  onSuggestions,
  onCheckMaterials,
}: Props) {
  const [busy, setBusy] = useState(false);
  const [checkingMaterials, setCheckingMaterials] = useState(false);

  const diagnose = async () => {
    if (!jobId) return;
    setBusy(true);
    try {
      const result = (await aec.render.diagnose(jobId)) as {
        suggestions?: DoctorSuggestion[];
      };
      onSuggestions(result.suggestions ?? []);
    } finally {
      setBusy(false);
    }
  };

  const checkMaterials = async () => {
    if (!onCheckMaterials) return;
    setCheckingMaterials(true);
    try {
      const found = await onCheckMaterials();
      if (found) onSuggestions(found);
    } finally {
      setCheckingMaterials(false);
    }
  };

  // Group: material findings (prefixed code) vs. render-time issues.
  const materialFindings = suggestions.filter((s) =>
    s.code.startsWith("material."),
  );
  const renderFindings = suggestions.filter(
    (s) => !s.code.startsWith("material."),
  );

  return (
    <section
      className="render-doctor"
      aria-label="Render doctor"
      data-testid="render-doctor"
    >
      <header>
        <span>{suggestions.length} suggestion(s)</span>
        <div className="render-doctor__actions">
          {onCheckMaterials && (
            <button
              type="button"
              disabled={checkingMaterials}
              data-testid="render-doctor-check-materials"
              onClick={checkMaterials}
            >
              {checkingMaterials ? "Checking…" : "Check Materials"}
            </button>
          )}
          <button
            type="button"
            disabled={!jobId || busy}
            data-testid="render-doctor-diagnose"
            onClick={diagnose}
          >
            {busy ? "Diagnosing…" : "Diagnose"}
          </button>
        </div>
      </header>
      {jobId === null && suggestions.length === 0 && (
        <p data-testid="render-doctor-empty">Select a job to diagnose.</p>
      )}
      {materialFindings.length > 0 && (
        <>
          <h3 className="render-doctor__group" data-testid="render-doctor-group-material">
            Materials
          </h3>
          <ul>
            {materialFindings.map((s, i) => (
              <li
                key={`mat-${i}`}
                className={`render-doctor__row render-doctor__row--${s.severity}`}
                data-testid={`render-doctor-material-${i}`}
              >
                <span className="render-doctor__code">{s.code}</span>
                {s.materialId && (
                  <span className="render-doctor__material">
                    {s.materialId}
                  </span>
                )}
                <span className="render-doctor__msg">{s.message}</span>
                {s.fix && (
                  <span className="render-doctor__fix">Fix: {s.fix}</span>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
      {renderFindings.length > 0 && (
        <ul>
          {renderFindings.map((s, i) => (
            <li
              key={`render-${i}`}
              className={`render-doctor__row render-doctor__row--${s.severity}`}
              data-testid={`render-doctor-item-${i}`}
            >
              <span className="render-doctor__code">{s.code}</span>
              <span className="render-doctor__msg">{s.message}</span>
              {s.fix && (
                <span className="render-doctor__fix">Fix: {s.fix}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
