/**
 * Deliver mode page.
 *
 * Hosts the pack composer, export-target list, revision manager, and
 * deliver toolbar. The page owns the wiring between revision creation,
 * compare, and pack export — components themselves remain dumb /
 * controlled.
 */

import { useEffect, useMemo, useState } from "react";

import { aec } from "../api/aec";
import type {
  RevisionSummary,
  VersionDiffSummary,
} from "../../../electron/bridge";
import {
  PackComposer,
  defaultDeliverablesFor,
  useReseedOnKindChange,
  type PackDeliverables,
  type PackKind,
} from "../components/deliver/PackComposer";
import {
  ExportTargetList,
  defaultExportTargets,
  type ExportTarget,
} from "../components/deliver/ExportTargetList";
import { RevisionManager } from "../components/deliver/RevisionManager";
import { DeliverToolbar } from "../components/deliver/DeliverToolbar";

export function Deliver(): JSX.Element {
  const [kind, setKind] = useState<PackKind>("concept");
  const [deliverables, setDeliverables] = useState<PackDeliverables>(() =>
    defaultDeliverablesFor("concept"),
  );

  const [targets, setTargets] = useState<ExportTarget[]>(() =>
    defaultExportTargets(),
  );
  const [selectedTargetId, setSelectedTargetId] = useState<string>(
    () => defaultExportTargets()[0].id,
  );

  const [revisions, setRevisions] = useState<RevisionSummary[]>([]);
  const [baseId, setBaseId] = useState<string | null>(null);
  const [headId, setHeadId] = useState<string | null>(null);
  const [diff, setDiff] = useState<VersionDiffSummary | null>(null);
  const [comparing, setComparing] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportResult, setExportResult] = useState<{
    outPath: string;
    contents: string[];
    totalBytes: number;
  } | null>(null);

  // Initial load of revisions from the backend.
  useEffect(() => {
    let alive = true;
    void aec.deliver.listRevisions().then((rs) => {
      if (alive) setRevisions(rs);
    });
    return () => {
      alive = false;
    };
  }, []);

  // Reseed deliverables when the kind changes. The exported hook keeps
  // the reseed logic colocated with `defaultDeliverablesFor` so the two
  // can't drift apart.
  useReseedOnKindChange(kind, setDeliverables);

  const selectedTarget = useMemo(
    () => targets.find((t) => t.id === selectedTargetId) ?? targets[0],
    [targets, selectedTargetId],
  );

  const canTag = true;
  const canCompare = baseId !== null && headId !== null && baseId !== headId;
  const canExport = selectedTarget !== undefined;

  const refreshRevisions = async () => {
    const rs = await aec.deliver.listRevisions();
    setRevisions(rs);
  };

  const onCreateRevision = async (tag: string, description: string) => {
    await aec.deliver.createRevision({
      tag,
      description,
      entities: [
        // Placeholder entity set — until aec_command wires the live
        // project graph through, we seed the revision with a couple of
        // canonical entities so the compare panel has something to
        // diff. The Rust side accepts arbitrary categories.
        {
          category: "manifest",
          id: "project",
          payloadHash: "00".repeat(32),
        },
      ],
    });
    await refreshRevisions();
  };

  const onCompare = async () => {
    if (!canCompare) return;
    setComparing(true);
    try {
      const d = await aec.deliver.compareRevisions({
        baseId: baseId!,
        headId: headId!,
      });
      setDiff(d);
    } finally {
      setComparing(false);
    }
  };

  const onBuildPack = async () => {
    if (!selectedTarget) return;
    setExporting(true);
    setExportResult(null);
    try {
      const result = await aec.deliver.buildPack({
        kind,
        outPath: selectedTarget.path,
        includeRenders: deliverables.renders,
        includeSheets: deliverables.sheets,
        includeIfc: deliverables.ifc,
        includeBoq: deliverables.boq,
        includeProposal: deliverables.proposal,
      });
      setExportResult(result);
    } finally {
      setExporting(false);
    }
  };

  return (
    <div data-testid="deliver-mode">
      <DeliverToolbar
        onExportPack={onBuildPack}
        onTagRevision={() => {
          /* The form lives inside RevisionManager — toolbar button
             focuses the tag input via the document API. */
          const el = document.querySelector<HTMLInputElement>(
            "[data-testid=\"revision-tag-input\"]",
          );
          el?.focus();
        }}
        onCompareRevisions={onCompare}
        exporting={exporting}
        canExport={canExport}
        canTag={canTag}
        canCompare={canCompare}
      />
      <div className="deliver-layout">
        <PackComposer
          kind={kind}
          deliverables={deliverables}
          onKindChange={setKind}
          onDeliverablesChange={setDeliverables}
          onBuild={onBuildPack}
          building={exporting}
        />
        <ExportTargetList
          targets={targets}
          selectedId={selectedTargetId}
          onSelect={setSelectedTargetId}
          onPathChange={(id, path) =>
            setTargets((prev) =>
              prev.map((t) => (t.id === id ? { ...t, path } : t)),
            )
          }
        />
        <RevisionManager
          revisions={revisions}
          baseId={baseId}
          headId={headId}
          onSelectBase={setBaseId}
          onSelectHead={setHeadId}
          onCreateRevision={onCreateRevision}
          onCompare={onCompare}
          diff={diff}
          comparing={comparing}
        />
      </div>
      {exportResult ? (
        <section data-testid="deliver-export-result">
          <h3>Pack built</h3>
          <p>
            Wrote {exportResult.contents.length} files (
            {Math.round(exportResult.totalBytes / 1024)} KB) to{" "}
            <code>{exportResult.outPath}</code>
          </p>
          <ul>
            {exportResult.contents.map((c) => (
              <li key={c} data-testid={`pack-file-${c}`}>
                {c}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}
