/**
 * Shared renderer-side adapter between the bridge's wire-format
 * `BimValidationFinding` (`description` / `element` / `suggestion`,
 * 1:1 with the Rust service struct) and the renderer-internal
 * `ValidationFinding` (`message` / `entityId` / `hint`, used by
 * `ValidatorPanel`'s UI).
 *
 * Both the `ValidatorPanel` "Re-validate" button and the demo
 * `Bim.tsx` page's "Validate" toolbar action consume this adapter so
 * the wire→UI mapping lives in exactly one place. An earlier
 * iteration inlined the mapping in both call sites, which Devin
 * Review flagged as drift bait (ANALYSIS_pr-T_0002); this module
 * is the canonical fix.
 */

export type ValidationSeverity = "error" | "warning" | "info";

export interface ValidationFinding {
  code: string;
  severity: ValidationSeverity;
  message: string;
  entityId?: string | null;
  hint?: string | null;
}

/**
 * Wire-format `BimValidationFinding` (mirrored from
 * `electron/bridge.ts`). Imported locally rather than re-exported
 * from the bridge so this module stays renderer-only and doesn't
 * drag the main-process surface into the renderer's type graph
 * (same Electron context-isolation rationale as `preload.ts`).
 */
export interface BimValidationFindingWire {
  severity: ValidationSeverity;
  code: string;
  element: string | null;
  description: string;
  suggestion: string | null;
}

/**
 * Convert a single wire-format finding to the renderer's
 * `ValidationFinding` shape.
 *
 * The wire format carries its own `severity` field, but it's
 * already implied by which of the bucketed arrays (`errors` /
 * `warnings` / `infos`) the finding came from. Accepting the
 * `severity` parameter explicitly avoids relying on a
 * round-trip-only field and matches the call pattern of
 * `bimReportToFindings` below.
 */
export function adaptValidationFinding(
  f: BimValidationFindingWire,
  severity: ValidationSeverity,
): ValidationFinding {
  return {
    code: f.code,
    severity,
    message: f.description,
    entityId: f.element,
    hint: f.suggestion,
  };
}

/**
 * Shape of the bridge's `BimValidateReport`. Mirrored locally for
 * the same reason as `BimValidationFindingWire` above.
 */
export interface BimValidateReportWire {
  errors: BimValidationFindingWire[];
  warnings: BimValidationFindingWire[];
  infos: BimValidationFindingWire[];
}

/**
 * Flatten a `BimValidateReport` (three bucketed arrays from the
 * bridge) into a single renderer-side `ValidationFinding[]`,
 * preserving the original severity tagging without trusting the
 * `severity` field on each finding.
 */
export function bimReportToFindings(
  report: BimValidateReportWire,
): ValidationFinding[] {
  return [
    ...report.errors.map((f) => adaptValidationFinding(f, "error")),
    ...report.warnings.map((f) => adaptValidationFinding(f, "warning")),
    ...report.infos.map((f) => adaptValidationFinding(f, "info")),
  ];
}
