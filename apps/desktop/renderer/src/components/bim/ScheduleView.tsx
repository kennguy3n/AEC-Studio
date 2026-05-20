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

interface Props {
  rowsByKind: Partial<Record<ScheduleKind, ScheduleRow[]>>;
  onGenerate: (kind: ScheduleKind, rows: ScheduleRow[]) => void;
}

export function ScheduleView({ rowsByKind, onGenerate }: Props) {
  const [active, setActive] = useState<ScheduleKind>("room");
  const [busy, setBusy] = useState(false);
  const rows = rowsByKind[active] ?? [];
  const columns = rows.length === 0 ? [] : Object.keys(rows[0]);

  const regenerate = async () => {
    setBusy(true);
    try {
      const result = (await aec.bim.generateSchedule({ kind: active })) as {
        scheduleId: string;
        rows?: ScheduleRow[];
      };
      onGenerate(active, result.rows ?? []);
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
