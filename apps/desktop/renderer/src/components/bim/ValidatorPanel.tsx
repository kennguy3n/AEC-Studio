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
  /**
   * Error reporter for bridge failures. Phase 13 wires the bridge
   * to real OS paths from the file picker, so `aec.bim.validate`
   * can reject with real errors (file moved/deleted between import
   * and revalidate, permission denied, malformed IFC on re-read,
   * locked DB). Without this callback the rejection would surface
   * as an unhandled promise rejection from `onClick` with zero
   * user feedback — no toast, no error-boundary trigger (async
   * rejections don't bubble to React error boundaries).
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
   * required in every test). Mirrors the `onError` contract on
   * `ScheduleView` so the two BIM panels share one failure shape.
   */
  onError?: (message: string) => void;
}

export function ValidatorPanel({
  sourcePath,
  findings,
  onFindings,
  onZoomTo,
  onError,
}: Props) {
  const [busy, setBusy] = useState(false);

  const revalidate = async () => {
    // Defense-in-depth: the button is already disabled when
    // `sourcePath === ""` or `busy === true`, but mirror the guard
    // here so any future non-button caller (parent-driven
    // re-validation on mount, a keyboard shortcut, a "validate all"
    // toolbar action) cannot send an empty path into
    // `aec.bim.validate`. The main-process IPC handler rejects it
    // via `assertString(sourcePath, "sourcePath")`, but the
    // rejection surfaces as an unhandled promise rejection with no
    // user feedback — and the in-process fallback accepts empty
    // strings silently, hiding the regression from unit tests.
    // Returning early here keeps the failure mode aligned with the
    // disabled-button UX. Mirrors the ScheduleView regenerate guard
    // so the two BIM panels share one contract.
    if (busy || sourcePath === "") {
      return;
    }
    setBusy(true);
    try {
      const result = await aec.bim.validate({ sourcePath });
      onFindings(bimReportToFindings(result));
    } catch (err) {
      // Surface the bridge failure through the parent's error
      // reporter. The bridge runs against real OS paths from the
      // file picker, so file-moved, permission-denied,
      // malformed-IFC, and locked-DB errors are real failure
      // surfaces. Without this catch the rejection would become
      // an unhandled promise rejection from `onClick` with no
      // user feedback (async rejections don't trigger React
      // error boundaries). The `finally` below still clears the
      // `busy` flag so the button re-enables for retry — the
      // failure is recoverable from the user's perspective.
      // Mirrors the catch in `ScheduleView.regenerate` so the
      // two BIM panels share one failure contract.
      const msg = err instanceof Error ? err.message : String(err);
      const display = `Re-validate failed: ${msg}`;
      if (onError) {
        onError(display);
      } else {
        // Dev fallback: parents that don't supply `onError` get a
        // console.error so the failure is still observable in
        // tests / future call sites instead of silently swallowed.
        console.error(display, err);
      }
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
          // Disabled when busy *or* the parent hasn't imported an IFC
          // yet (sourcePath === ""). Without the empty-string guard
          // the bridge would receive an empty path and fail in the
          // main-process IPC handler at `assertString(sourcePath,
          // "sourcePath")` (ipc.ts:262), producing an unhandled
          // promise rejection with no user feedback. The in-process
          // fallback (`renderer-backend.ts`) accepts empty strings
          // silently, which is why the regression isn't visible in
          // unit tests — the disabled state is the user-facing fix.
          // Mirrors the ScheduleView regenerate-button guard.
          disabled={busy || sourcePath === ""}
          title={sourcePath === "" ? "Import an IFC first" : undefined}
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
