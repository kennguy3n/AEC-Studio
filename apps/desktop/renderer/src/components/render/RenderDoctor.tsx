import { useState } from "react";
import { aec } from "../../api/aec";

export interface DoctorSuggestion {
  code: string;
  severity: "info" | "warning" | "error";
  message: string;
  fix?: string | null;
}

interface Props {
  jobId: string | null;
  suggestions: DoctorSuggestion[];
  onSuggestions: (next: DoctorSuggestion[]) => void;
}

export function RenderDoctor({ jobId, suggestions, onSuggestions }: Props) {
  const [busy, setBusy] = useState(false);

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

  return (
    <section
      className="render-doctor"
      aria-label="Render doctor"
      data-testid="render-doctor"
    >
      <header>
        <span>{suggestions.length} suggestion(s)</span>
        <button
          type="button"
          disabled={!jobId || busy}
          data-testid="render-doctor-diagnose"
          onClick={diagnose}
        >
          {busy ? "Diagnosing…" : "Diagnose"}
        </button>
      </header>
      {jobId === null && (
        <p data-testid="render-doctor-empty">Select a job to diagnose.</p>
      )}
      <ul>
        {suggestions.map((s, i) => (
          <li
            key={i}
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
    </section>
  );
}
