import { useState } from "react";
import { aec } from "../../api/aec";

export type ScheduleKind = "room" | "door" | "window" | "material";

const TABS: { id: ScheduleKind; label: string }[] = [
  { id: "room", label: "Rooms" },
  { id: "door", label: "Doors" },
  { id: "window", label: "Windows" },
  { id: "material", label: "Materials" },
];

export interface ScheduleRow {
  [column: string]: string | number;
}

/**
 * Summary returned to the parent after "Regenerate". The bridge
 * writes the schedule directly to an XLSX file at `outPath` (via
 * `ScheduleSheet::write_xlsx` in `aec_bim`), so the renderer just
 * surfaces the file path and row/column counts — the inline
 * preview table reflects whatever rows the parent retains in
 * `rowsByKind` (typically empty until a future PR adds an
 * XLSX-to-row-list parse step).
 */
export interface ScheduleGenerationSummary {
  scheduleId: string;
  outPath: string;
  rows: number;
  columns: number;
  bytesWritten: number;
}

interface Props {
  /** IFC path to read when "Regenerate" is clicked. */
  sourcePath: string;
  /** Path the bridge writes the XLSX to when "Regenerate" is clicked. */
  outPathForKind: (kind: ScheduleKind) => string;
  rowsByKind: Partial<Record<ScheduleKind, ScheduleRow[]>>;
  onGenerate: (kind: ScheduleKind, summary: ScheduleGenerationSummary) => void;
}

export function ScheduleView({
  sourcePath,
  outPathForKind,
  rowsByKind,
  onGenerate,
}: Props) {
  const [active, setActive] = useState<ScheduleKind>("room");
  const [busy, setBusy] = useState(false);
  const rows = rowsByKind[active] ?? [];
  const columns = rows.length === 0 ? [] : Object.keys(rows[0]);

  const regenerate = async () => {
    setBusy(true);
    try {
      const outPath = outPathForKind(active);
      const result = await aec.bim.generateSchedule({
        sourcePath,
        outPath,
        kind: active,
      });
      onGenerate(active, {
        scheduleId: result.scheduleId,
        outPath: result.outPath,
        rows: result.rows,
        columns: result.columns,
        bytesWritten: result.bytesWritten,
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className="bim-schedule"
      aria-label="Schedule view"
      data-testid="schedule-view"
    >
      <div className="bim-schedule__tabs" role="tablist">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={active === tab.id}
            className={`bim-schedule__tab${active === tab.id ? " bim-schedule__tab--active" : ""}`}
            data-testid={`schedule-tab-${tab.id}`}
            onClick={() => setActive(tab.id)}
          >
            {tab.label}
          </button>
        ))}
        <button
          type="button"
          className="bim-schedule__regen"
          data-testid="schedule-regenerate"
          onClick={regenerate}
          disabled={busy}
        >
          {busy ? "Generating…" : "Regenerate"}
        </button>
      </div>
      <div className="bim-schedule__body">
        {rows.length === 0 ? (
          <p className="bim-schedule__empty" data-testid="schedule-empty">
            No rows yet for this schedule. Click "Regenerate" to compute from
            the current model.
          </p>
        ) : (
          <table>
            <thead>
              <tr>
                {columns.map((c) => (
                  <th key={c} scope="col">
                    {c}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody data-testid={`schedule-rows-${active}`}>
              {rows.map((row, i) => (
                <tr key={i}>
                  {columns.map((c) => (
                    <td key={c}>{String(row[c] ?? "")}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}
