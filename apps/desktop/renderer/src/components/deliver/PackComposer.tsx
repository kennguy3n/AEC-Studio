/**
 * Pack composer panel for Deliver mode.
 *
 * Lets the user pick a pack kind (concept / interior / contractor /
 * BIM-lite) and toggle which deliverable categories go into the pack.
 * Each pack kind has a different default set of categories — toggling
 * the kind reseeds the checkboxes. The kind picker is the source of
 * truth for which deliverable categories are even applicable (e.g.
 * a BIM-lite pack always includes IFC, an interior pack never does).
 */

import { useEffect, useMemo } from "react";

export type PackKind = "concept" | "interior" | "contractor" | "bim";

export interface PackDeliverables {
  renders: boolean;
  sheets: boolean;
  ifc: boolean;
  boq: boolean;
  proposal: boolean;
}

export interface PackComposerProps {
  kind: PackKind;
  deliverables: PackDeliverables;
  onKindChange: (kind: PackKind) => void;
  onDeliverablesChange: (next: PackDeliverables) => void;
  onBuild: () => void;
  building?: boolean;
}

interface PackKindMeta {
  id: PackKind;
  label: string;
  description: string;
  applicable: Array<keyof PackDeliverables>;
}

const PACK_KINDS: PackKindMeta[] = [
  {
    id: "concept",
    label: "Client concept",
    description: "Cover + plan + renders + schedule for client review",
    applicable: ["renders", "sheets"],
  },
  {
    id: "interior",
    label: "Interior package",
    description: "PDF summary + render gallery + material schedule",
    applicable: ["renders"],
  },
  {
    id: "contractor",
    label: "Contractor handoff",
    description: "Sheets + schedules + IFC + BOQ + manifest",
    applicable: ["sheets", "boq", "ifc", "proposal"],
  },
  {
    id: "bim",
    label: "BIM Lite",
    description: "IFC + sheets + validation report",
    applicable: ["sheets", "ifc"],
  },
];

const DELIVERABLE_LABELS: Array<{ id: keyof PackDeliverables; label: string }> = [
  { id: "renders", label: "Renders" },
  { id: "sheets", label: "Sheets" },
  { id: "ifc", label: "IFC model" },
  { id: "boq", label: "BOQ schedule" },
  { id: "proposal", label: "Proposal PDF" },
];

export function PackComposer({
  kind,
  deliverables,
  onKindChange,
  onDeliverablesChange,
  onBuild,
  building = false,
}: PackComposerProps): JSX.Element {
  const meta = useMemo(() => {
    return PACK_KINDS.find((p) => p.id === kind) ?? PACK_KINDS[0];
  }, [kind]);

  return (
    <section data-testid="pack-composer">
      <header>
        <h2>Pack composer</h2>
        <p>Pick a pack kind, then toggle deliverables. Defaults match the kind.</p>
      </header>
      <fieldset>
        <legend>Pack kind</legend>
        <div role="radiogroup" aria-label="Pack kind">
          {PACK_KINDS.map((k) => (
            <label key={k.id} data-testid={`pack-kind-${k.id}`}>
              <input
                type="radio"
                name="pack-kind"
                value={k.id}
                checked={kind === k.id}
                onChange={() => onKindChange(k.id)}
              />
              <span>{k.label}</span>
              <small>{k.description}</small>
            </label>
          ))}
        </div>
      </fieldset>
      <fieldset>
        <legend>Deliverables</legend>
        {DELIVERABLE_LABELS.map((d) => {
          const applicable = meta.applicable.includes(d.id);
          return (
            <label key={d.id} data-testid={`pack-deliverable-${d.id}`}>
              <input
                type="checkbox"
                disabled={!applicable}
                checked={applicable && deliverables[d.id]}
                onChange={(e) =>
                  onDeliverablesChange({
                    ...deliverables,
                    [d.id]: e.target.checked,
                  })
                }
              />
              <span>{d.label}</span>
              {!applicable && (
                <small data-testid={`pack-disabled-${d.id}`}>
                  not in this pack
                </small>
              )}
            </label>
          );
        })}
      </fieldset>
      <button
        type="button"
        onClick={onBuild}
        disabled={building}
        data-testid="pack-build"
      >
        {building ? "Building…" : "Build pack"}
      </button>
    </section>
  );
}

/**
 * Default `PackDeliverables` for a pack kind. Used by the page when
 * the user switches kinds so the checkboxes seed sensibly.
 */
export function defaultDeliverablesFor(kind: PackKind): PackDeliverables {
  switch (kind) {
    case "concept":
      return {
        renders: true,
        sheets: true,
        ifc: false,
        boq: false,
        proposal: false,
      };
    case "interior":
      return {
        renders: true,
        sheets: false,
        ifc: false,
        boq: false,
        proposal: false,
      };
    case "contractor":
      return {
        renders: false,
        sheets: true,
        ifc: true,
        boq: true,
        proposal: true,
      };
    case "bim":
      return {
        renders: false,
        sheets: true,
        ifc: true,
        boq: false,
        proposal: false,
      };
  }
}

/**
 * Internal hook used by `Deliver.tsx` to keep checkbox state in sync
 * with the selected pack kind. When the kind changes we reseed the
 * deliverables; the user can then opt out of any category.
 */
export function useReseedOnKindChange(
  kind: PackKind,
  setDeliverables: (next: PackDeliverables) => void,
): void {
  useEffect(() => {
    setDeliverables(defaultDeliverablesFor(kind));
  }, [kind, setDeliverables]);
}
