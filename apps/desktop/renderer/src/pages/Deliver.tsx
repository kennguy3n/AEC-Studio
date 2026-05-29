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
import { useActiveProject } from "../hooks/useActiveProject";
import { useToast } from "../hooks/useToast";
import type {
  RevisionSummary,
  VersionDiffSummary,
} from "../../../electron/bridge";
import { KChatReviewPanel } from "../components/kchat/KChatReviewPanel";
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

/**
 * Fallback thread id used when no per-project `KChatConfig` has
 * been adopted by the bridge yet (no project open, or the
 * manifest left `default_thread_id` unset). Mirrors
 * `DEFAULT_THREAD_ID` in `crates/aec_core/src/kchat.rs` so the
 * Deliver review panel and the Rust-side publisher converge on
 * the same default thread.
 */
const FALLBACK_THREAD_ID = "kchat-default";

/**
 * Cadence at which the Deliver page re-reads
 * `kchat:status.defaultThreadId` to pick up project switches /
 * KChatConfig edits that happened after the page mounted. Matches
 * the cadence used by `KChatStatusIndicator` so the two converge on
 * the same view of the bridge state.
 */
const STATUS_POLL_INTERVAL_MS = 5_000;

export function Deliver(): JSX.Element {
  const { project, getActiveProjectPath, getActiveProject } = useActiveProject();
  const { addToast } = useToast();
  const [kind, setKind] = useState<PackKind>("concept");
  const [deliverables, setDeliverables] = useState<PackDeliverables>(() =>
    defaultDeliverablesFor("concept"),
  );

  // Per-project KChat thread id, polled from `kchat:status` so the
  // Deliver review panel keys off the active project's
  // `KChatConfig::default_thread_id` rather than a hard-coded value.
  // Falls back to `FALLBACK_THREAD_ID` (matching the bridge
  // publisher's `DEFAULT_THREAD_ID` constant) when no project is
  // open / the project chose to leave the field unset.
  const [defaultThreadId, setDefaultThreadId] = useState<string | null>(null);

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

  // Defense-in-depth against project switches racing in-flight
  // `compareRevisions` / `createRevision` / `buildPack` work. Each
  // handler captures `startPath` at entry and re-checks
  // `getActiveProjectPath()` after every await; on mismatch, the
  // handler skips the stale `setState` / toast commit so project A's
  // bridge response can never land into project B's state.
  //
  //   * Today the `RequireProject` route guard unmounts Deliver on
  //     every project transition, so a stale `setDiff` / `setRevisions`
  //     / `setExportResult` would be a no-op on a torn-down component
  //     (React 18 silently discards updates to unmounted nodes — no
  //     warning since the strict-mode warning was removed). The page
  //     is safe in production.
  //   * BUT the route-guard umbrella is the same brittle contract
  //     the per-project reset effect (lines below) chose not to rely
  //     on. Future in-page project pickers, "switch to recent"
  //     toolbar actions, or any code path that calls
  //     `openProject(...)` without forcing a route change would let
  //     project A's bridge response land into project B's state.
  //
  // The page used to mirror `project?.path` into a local
  // `useRef + useEffect`; that pattern had a one-render-cycle lag
  // versus `useActiveProject`'s internal synchronous ref (see the
  // `getActiveProjectPath` docblock on `ActiveProjectState` for
  // the full timing analysis). Reading through `getActiveProjectPath()`
  // on every guard site routes the check through the central sync
  // ref so every consumer reads the same source of truth at the
  // same moment — matching the pattern landed in `Bim.tsx`,
  // `Render.tsx`, and the internal guard in
  // `useActiveProject.saveProject`.

  // Per-project state reset + revisions load on project switch.
  //
  // The `revisions`, `diff`, `baseId`, `headId`, and `exportResult`
  // state are all *per-project*: revision IDs are not valid across
  // projects (the deliver-store keys them on the project root, so a
  // stale ID from project A passed to `aec.deliver.compareRevisions`
  // on project B would either resolve to the wrong revision or be
  // rejected by the bridge); `diff` and `exportResult` reference
  // revision IDs by string so they share the same fate; the
  // base/head selection drives the compare button enablement and is
  // meaningless against a different revision list.
  //
  // Today the `RequireProject` route guard unmounts the Deliver page
  // on every project transition, which `useState`-resets the page for
  // free — so this effect is *defense-in-depth*. The route-guard
  // umbrella is the same brittle contract that `Bim.tsx:111-118` and
  // `Render.tsx:91-165` chose not to rely on: future in-page project
  // pickers, "switch to recent" toolbar actions, or any code path
  // that calls `openProject(...)` without forcing a route change
  // would silently leak project A's revisions / diff into project B
  // without this reset. The synchronous clear runs *before* the
  // async `listRevisions()` so the reset is observable before the
  // bridge response arrives, mirroring the `setCameras(EMPTY)` /
  // `setJobs([])` pattern in Render.tsx that closes the
  // one-microtask flash window. `comparing` and `exporting` are also
  // reset because their `true` value would otherwise prevent the
  // user from re-enqueuing compare / export against the new project.
  //
  // The `listRevisions()` call is unconditional (not gated on
  // `project?.path`). In production the page is route-guarded by
  // `RequireProject` so a missing active project is impossible — the
  // bridge would throw `no project is currently open` anyway. In the
  // vitest fixture the in-process deliver mock returns the shared
  // revisions array regardless of project state, and several smoke
  // tests in `Deliver.test.tsx` mount the page directly (without
  // `RequireProject`) and rely on listRevisions surfacing fixture
  // state. The bridge-side `getActiveProjectPath` validation is the
  // single point of enforcement; this hook should not duplicate it.
  //
  // Keyed on `project?.path` (matching Render.tsx, StatusBar.tsx, and
  // Bim.tsx) rather than `project` so the effect does NOT re-fire on
  // every save — `updateProject(summary)` runs after each save and
  // recreates the summary reference, which would otherwise tear down
  // and recreate the listRevisions call on every 5s auto-save.
  useEffect(() => {
    let alive = true;
    setRevisions([]);
    setBaseId(null);
    setHeadId(null);
    setDiff(null);
    setExportResult(null);
    setComparing(false);
    setExporting(false);
    // Also reset `defaultThreadId` synchronously here so the
    // KChatReviewPanel doesn't render project A's thread for up to
    // one `STATUS_POLL_INTERVAL_MS` window after switching to
    // project B. The polling effect below re-keys on `project?.path`
    // and fires an immediate re-poll on switch, but the previous
    // poll's resolved value would otherwise survive until the new
    // poll's promise resolves (typically <1 frame, but a slow
    // bridge could stretch this to seconds). Falling back to
    // `FALLBACK_THREAD_ID` (the same constant the bridge publisher
    // uses when `default_thread_id` is unset on a fresh project)
    // keeps the panel functional during the brief gap between
    // reset and first new-project poll resolution.
    setDefaultThreadId(null);
    void aec.deliver
      .listRevisions()
      .then((rs) => {
        if (alive) setRevisions(rs);
      })
      .catch(() => {
        // Bridge failure (corrupt DB, permission denied, project
        // closed mid-poll) — keep the empty list (synchronously
        // reset above) so the user sees the empty-state instead
        // of stale entries from a previous project. Matches the
        // Render.tsx:156 listGraph pattern. The bridge layer logs
        // the underlying error; the renderer does not surface a
        // toast because (a) `RequireProject` already gates this
        // page, (b) routine project switches race the bridge's
        // own teardown and would otherwise spam toasts, and (c)
        // the empty-state UI in RevisionManager already
        // communicates "no revisions available".
      });
    return () => {
      alive = false;
    };
  }, [project?.path]);

  // Poll the bridge for the per-project KChat thread id. Mirrors the
  // cadence used by `KChatStatusIndicator` so the Deliver review
  // panel converges on the same thread the status chip displays.
  // Failures are swallowed: the panel keeps showing the last-known
  // thread (or the fallback constant) rather than flickering on a
  // single missed poll.
  //
  // Re-keyed on `project?.path` so a project switch fires an
  // immediate re-poll (the previous interval is torn down by the
  // cleanup return). Without this re-keying, the polling effect's
  // mount-only deps `[]` meant a switch from project A to project
  // B would wait up to `STATUS_POLL_INTERVAL_MS` (5 seconds) for
  // the next tick to fetch project B's thread — during which
  // `KChatReviewPanel` would render project A's thread (or the
  // fallback after the per-project reset effect's
  // `setDefaultThreadId(null)` above). Re-keying converts that
  // 5-second gap to a single bridge round-trip (~1 frame in
  // production, instant in the vitest in-process backend).
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const s = await aec.kchat.status();
        if (!cancelled) {
          setDefaultThreadId(s.defaultThreadId ?? null);
        }
      } catch {
        // Intentionally swallowed — see comment above.
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), STATUS_POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [project?.path]);

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

  // `refreshRevisions` is called both by the initial-mount effect and
  // by `onCreateRevision`. The handler-driven path needs a project
  // guard so a project switch racing an in-flight `createRevision`
  // doesn't refresh project A's revisions onto project B's state.
  // The caller captures `startPath` and threads it through so the
  // guard is enforced at the setState site rather than relying on
  // the caller to re-check.
  const refreshRevisions = async (startPath: string | null) => {
    const rs = await aec.deliver.listRevisions();
    if (getActiveProjectPath() !== startPath) return;
    setRevisions(rs);
  };

  const onCreateRevision = async (tag: string, description: string) => {
    const startPath = getActiveProjectPath();
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
    // Skip the refresh if the user project-switched while the
    // createRevision bridge call was in flight. The new project's
    // per-project reset effect (lines 132-162) already cleared
    // revisions/baseId/headId, and `refreshRevisions` would otherwise
    // overwrite that clean state with project A's revisions.
    if (getActiveProjectPath() !== startPath) return;
    await refreshRevisions(startPath);
  };

  const onCompare = async () => {
    if (!canCompare) return;
    const startPath = getActiveProjectPath();
    setComparing(true);
    try {
      const d = await aec.deliver.compareRevisions({
        baseId: baseId!,
        headId: headId!,
      });
      // Skip stale: project A's diff must not land into project B's
      // state. The per-project reset effect already cleared diff to
      // null on the switch; this just prevents the in-flight result
      // from overwriting that.
      if (getActiveProjectPath() !== startPath) return;
      setDiff(d);
    } catch (err) {
      // Surface compare failures via toast so the user knows why no
      // diff appeared. Without this, a bridge rejection (corrupt
      // revision row, permission denied on the deliver-store DB,
      // locked SQLCipher transaction) would surface as a silent
      // "Uncaught (in promise)" in the renderer console while the
      // UI just sat with the previous diff. Mirrors the
      // `addToast("error", ...)` pattern used by `onBuildPack`
      // below (and `Bim.tsx onInvoke`'s outer catch) so all three
      // async handlers on this page share one user-visible error
      // contract.
      //
      // Toast unconditionally — errors are project-agnostic UX.
      // Even if the user project-switched while the
      // `compareRevisions` bridge call was in flight, the failure
      // still describes "your last compare action failed" which is
      // useful regardless of the active project. Matches
      // `onBuildPack`'s catch (which also toasts unconditionally),
      // diverging only from the success-path setState which IS
      // gated on path-match (a stale success result on project B
      // would mislead, but a stale failure tells the user their
      // recent action did not produce a diff — still accurate).
      addToast(
        "error",
        `Compare failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      // Mirror the path guard: if the project switched, the per-
      // project reset effect already cleared `comparing` to false,
      // so we must NOT re-set it (a redundant write would tear down
      // a newly-started compare on the new project that may have
      // begun before this handler's finally fires).
      if (getActiveProjectPath() === startPath) {
        setComparing(false);
      }
    }
  };

  const onBuildPack = async () => {
    if (!selectedTarget) return;
    // Capture BOTH the sync-ref path AND the sync-ref summary at
    // handler entry. Reading `project?.path` / `project?.name` from
    // the closure here would see the value as of the render that
    // produced this onBuildPack identity; if a project transition
    // landed between that render and this handler firing, the
    // closure-captured fields would describe a different project
    // than the `getActiveProjectPath()` checks below detect. Using
    // the sync getters makes every read in this handler — both the
    // guard comparisons and the bridge-call arguments — share one
    // source of truth (`projectPathRef` / `projectSummaryRef`),
    // which `updateProject` writes synchronously at the same call
    // site so they cannot drift. Matches the `startPath` pattern
    // landed in `Bim.tsx onInvoke` (see the long-form rationale
    // there for the timing analysis).
    const startPath = getActiveProjectPath();
    const startProject = getActiveProject();
    // Open a save dialog so the user can choose the output path.
    const dialog = await aec.dialog.saveFile({
      title: `Export ${kind} pack`,
      defaultPath: selectedTarget.path,
      filters: [{ name: "ZIP Archives", extensions: ["zip"] }],
    });
    if (dialog.canceled || !dialog.path) return;
    // If the user project-switched while the save dialog was open,
    // the dialog's selected path is for project A's output but the
    // active project is now B. Honoring the export would write
    // project A's pack against project B's bridge state — worse,
    // the success toast would announce a write that happened
    // against the wrong project. Abort the export entirely; the
    // user can re-trigger from the new project's Deliver page.
    if (getActiveProjectPath() !== startPath) return;
    setExporting(true);
    setExportResult(null);
    try {
      const result = await aec.deliver.buildPack({
        kind,
        outPath: dialog.path,
        // Source `projectPath` / `projectName` from the sync-ref
        // captures at handler entry (`startPath` / `startProject`)
        // rather than the closure-captured `project?.path` /
        // `project?.name`. The current code path has no `await`
        // between the post-dialog `getActiveProjectPath() !== startPath`
        // guard above and this bridge call, so a closure read would
        // also be safe today — but mixing sources between the guard
        // (sync ref) and the bridge args (closure) is the exact
        // inconsistency Devin Review flagged in `Bim.tsx onInvoke`
        // and that we just closed there. Threading `startPath` /
        // `startProject` keeps Deliver structurally aligned with BIM
        // so a future refactor that introduces an `await` between
        // the guard and the call (e.g. a pre-export validation
        // round-trip, a confirmation dialog, telemetry submit) does
        // not silently re-open the race window.
        //
        // Threading the project explicitly (not relying on the IPC
        // handler's `peekActiveProjectPath()` fallback) also matches
        // the pattern every other page (BIM, Draft, Render) follows:
        // the page *owns* the active-project reference via
        // `useActiveProject`, and the IPC handler's fallback is
        // defense-in-depth, not the primary path. Without this, a
        // future refactor that drops the IPC fallback (e.g. to
        // support multi-project workspaces) would silently break
        // Deliver export but no other page.
        projectPath: startPath ?? undefined,
        projectName: startProject?.name,
        includeRenders: deliverables.renders,
        includeSheets: deliverables.sheets,
        includeIfc: deliverables.ifc,
        includeBoq: deliverables.boq,
        includeProposal: deliverables.proposal,
      });
      // Skip stale result + toast: if the user project-switched
      // mid-export, the result describes a pack built against
      // project A's bridge state — surfacing it on project B's
      // page would mislead the user. The pack itself still wrote
      // to the user-chosen path; this just declines to announce it.
      if (getActiveProjectPath() !== startPath) return;
      setExportResult(result);
      addToast(
        "success",
        `${kind} pack exported: ${result.contents.length} files`,
      );
    } catch (err) {
      // Errors are project-agnostic UX: even after a project
      // switch, a failure to write the file is still useful to the
      // user ("your last action failed"). Toast unconditionally.
      addToast(
        "error",
        `Pack export failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      // Same rationale as onCompare's finally: skip the setState if
      // the project switched.
      if (getActiveProjectPath() === startPath) {
        setExporting(false);
      }
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
      <KChatReviewPanel threadId={defaultThreadId ?? FALLBACK_THREAD_ID} />
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
