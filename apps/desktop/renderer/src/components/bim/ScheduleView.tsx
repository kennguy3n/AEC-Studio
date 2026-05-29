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
 * surfaces the file path and row/column counts. The inline
 * preview table renders whatever rows the parent retains in
 * `rowsByKind`; `Bim.tsx` reads them back from the just-written
 * XLSX via `aec.bim.readScheduleRows({ xlsxPath })` so the table
 * mirrors the file on disk.
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
  /**
   * Error reporter for bridge failures. Phase 13 wires the bridge
   * to real OS paths from the file picker, so `aec.bim.generateSchedule`
   * can reject with real errors (file moved between import and
   * regenerate, disk full, locked DB, permission denied). Without
   * this callback the rejection would surface as an unhandled
   * promise rejection from `onClick` with zero user feedback — no
   * toast, no error-boundary trigger (async rejections don't bubble
   * to React error boundaries). Pre-Phase 13 every branch hit
   * `demo://...` paths that the in-process fallback never throws
   * for, which is why the missing catch was benign before.
   *
   * Receives a human-readable error message. The parent (`Bim.tsx`)
   * wires this to its `addToast("error", message)` so the failure
   * is reported through the same toast system as the toolbar's
   * centralized `onInvoke` catch. Optional so isolated unit tests
   * that don't exercise the failure path don't need to wire a
   * dependency — when omitted the panel falls back to
   * `console.error` so the failure is still observable in dev
   * builds rather than silently swallowed.
   *
   * Dependency-injection rather than direct `useToast` usage so
   * the panel stays a pure presentation component (no implicit
   * coupling to the toast provider tree, no provider wrapping
   * required in every test).
   */
  onError?: (message: string) => void;
}

export function ScheduleView({
  sourcePath,
  outPathForKind,
  rowsByKind,
  onGenerate,
  onError,
}: Props) {
  const [active, setActive] = useState<ScheduleKind>("room");
  const [busy, setBusy] = useState(false);
  const rows = rowsByKind[active] ?? [];
  const columns = rows.length === 0 ? [] : Object.keys(rows[0]);

  const regenerate = async () => {
    // Defense-in-depth: the button is already disabled when
    // `sourcePath === ""` (no IFC imported yet) or `busy === true`,
    // but mirror the guard here so any future non-button caller
    // (parent-driven re-generation on mount, a keyboard shortcut,
    // a "regenerate all" toolbar action) cannot send an empty path
    // into `aec.bim.generateSchedule`. The main-process IPC handler
    // would reject it via `assertString(sourcePath, "sourcePath")`,
    // but the rejection surfaces as an unhandled promise rejection
    // with no user feedback — and the in-process fallback accepts
    // empty strings silently, hiding the regression from unit tests.
    // Returning early here keeps the failure mode aligned with the
    // disabled-button UX. Mirrors the ValidatorPanel revalidate
    // guard so the two BIM panels share one contract.
    if (busy || sourcePath === "") {
      return;
    }
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
    } catch (err) {
      // Surface the bridge failure through the parent's error
      // reporter. Pre-Phase 13 the in-process fallback never
      // threw (every branch routed through `demo://`), so the
      // missing catch was benign — but Phase 13 wires real OS
      // paths from the file picker, so disk-full, permission
      // denied, locked-DB, and missing-file errors are real
      // failure surfaces. Without this catch the rejection would
      // become an unhandled promise rejection from `onClick` with
      // no user feedback (async rejections don't trigger React
      // error boundaries). The `finally` below still clears the
      // `busy` flag so the button re-enables for retry — the
      // failure is recoverable from the user's perspective.
      // Mirrors the catch in `ValidatorPanel.revalidate` so the
      // two BIM panels share one failure contract.
      const msg = err instanceof Error ? err.message : String(err);
      const display = `Regenerate ${active} schedule failed: ${msg}`;
      if (onError) {
        onError(display);
      } else {
        // Dev fallback: parents that don't supply `onError` get a
        // console.error so the failure is still observable in
        // tests / future call sites instead of silently swallowed.
        // Wrapped in a guard so production builds with `console`
        // shimmed to a no-op (e.g., for log forwarding) don't crash.
        console.error(display, err);
      }
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
          // Disabled when busy *or* the parent hasn't imported an IFC
          // yet (sourcePath === ""). Without the empty-string guard the
          // bridge would receive an empty path and fail on the native
          // side; the in-process fallback hides the regression in
          // tests, so the disabled state is the user-facing fix.
          disabled={busy || sourcePath === ""}
          title={sourcePath === "" ? "Import an IFC first" : undefined}
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
