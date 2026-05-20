/**
 * Export target list — picks the output format + path for a pack.
 *
 * Targets are presented as a flat list with one selected at a time;
 * editing the path updates the corresponding target. Targets advertise
 * format-specific defaults (`.pdf`, `.zip`, `.xlsx`) so the user
 * doesn't have to type the extension manually.
 */

import { useEffect, useState } from "react";

export type ExportFormat = "pdf" | "zip" | "xlsx";

export interface ExportTarget {
  id: string;
  label: string;
  format: ExportFormat;
  /** Path the file will be written to. Editable. */
  path: string;
}

export interface ExportTargetListProps {
  targets: ExportTarget[];
  selectedId: string;
  onSelect: (id: string) => void;
  onPathChange: (id: string, path: string) => void;
}

const FORMAT_LABEL: Record<ExportFormat, string> = {
  pdf: "PDF",
  zip: "ZIP",
  xlsx: "XLSX",
};

export function ExportTargetList({
  targets,
  selectedId,
  onSelect,
  onPathChange,
}: ExportTargetListProps): JSX.Element {
  // Local cache of the path edit so we don't fire onPathChange on
  // every keystroke during typing — only on blur.
  const [draftPath, setDraftPath] = useState<string>(() => {
    return targets.find((t) => t.id === selectedId)?.path ?? "";
  });

  // Re-seed the draft path whenever the selected target changes.
  useEffect(() => {
    const target = targets.find((t) => t.id === selectedId);
    setDraftPath(target?.path ?? "");
  }, [selectedId, targets]);

  return (
    <section data-testid="export-target-list">
      <header>
        <h2>Export targets</h2>
      </header>
      <ul>
        {targets.map((t) => (
          <li key={t.id} data-testid={`export-target-${t.id}`}>
            <label>
              <input
                type="radio"
                name="export-target"
                value={t.id}
                checked={t.id === selectedId}
                onChange={() => onSelect(t.id)}
              />
              <span>{t.label}</span>
              <small>{FORMAT_LABEL[t.format]}</small>
            </label>
          </li>
        ))}
      </ul>
      <label data-testid="export-target-path">
        Output path
        <input
          type="text"
          value={draftPath}
          onChange={(e) => setDraftPath(e.target.value)}
          onBlur={() => onPathChange(selectedId, draftPath)}
          aria-label="Output path"
        />
      </label>
    </section>
  );
}

/**
 * Default targets for the four pack kinds. Used to seed the page on
 * first mount; the user can edit the path afterwards.
 */
export function defaultExportTargets(): ExportTarget[] {
  return [
    {
      id: "concept",
      label: "Client concept pack",
      format: "pdf",
      path: "/exports/concept-pack.pdf",
    },
    {
      id: "interior",
      label: "Interior package",
      format: "zip",
      path: "/exports/interior-pack.zip",
    },
    {
      id: "contractor",
      label: "Contractor handoff",
      format: "zip",
      path: "/exports/contractor-pack.zip",
    },
    {
      id: "bim",
      label: "BIM Lite pack",
      format: "zip",
      path: "/exports/bim-pack.zip",
    },
  ];
}
