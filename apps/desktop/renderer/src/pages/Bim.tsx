import { useEffect, useState } from "react";
import { aec } from "../api/aec";
import { importIfcWithSizeGuard } from "../api/bim-import";
import { attachIfcToProject } from "../api/bim-attach";
import {
  SpatialTree,
  SpatialNode,
} from "../components/bim/SpatialTree";
import {
  PropertyEditor,
  PsetData,
} from "../components/bim/PropertyEditor";
import {
  ScheduleView,
  ScheduleKind,
  ScheduleRow,
} from "../components/bim/ScheduleView";
import { ValidatorPanel } from "../components/bim/ValidatorPanel";
import {
  bimReportToFindings,
  type ValidationFinding,
} from "../api/bim-validation";
import {
  BimToolbar,
  BimAction,
} from "../components/bim/BimToolbar";
import { useActiveProject } from "../hooks/useActiveProject";
import { useToast } from "../hooks/useToast";

const DEMO_ROOT: SpatialNode = {
  id: "proj_demo",
  kind: "IfcProject",
  name: "Project (demo)",
  children: [
    {
      id: "site_demo",
      kind: "IfcSite",
      name: "Site 1",
      children: [
        {
          id: "bldg_demo",
          kind: "IfcBuilding",
          name: "Building A",
          children: [
            {
              id: "lvl_l1",
              kind: "IfcBuildingStorey",
              name: "L1",
              children: [
                {
                  id: "spc_l1_living",
                  kind: "IfcSpace",
                  name: "Living",
                  children: [],
                },
              ],
            },
          ],
        },
      ],
    },
  ],
};

const DEMO_PSETS: PsetData = {
  Pset_WallCommon: {
    LoadBearing: true,
    FireRating: "F60",
  },
};

const IFC_FILTERS = [{ name: "IFC Files", extensions: ["ifc"] }];

export function Bim() {
  const { project, getActiveProjectPath } = useActiveProject();
  const { addToast } = useToast();
  const [root, setRoot] = useState<SpatialNode | null>(DEMO_ROOT);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [psets, setPsets] = useState<PsetData>(DEMO_PSETS);
  const [schedules, setSchedules] = useState<
    Partial<Record<ScheduleKind, ScheduleRow[]>>
  >({});
  const [findings, setFindings] = useState<ValidationFinding[]>([]);
  const [busyAction, setBusyAction] = useState<BimAction | null>(null);

  // The IFC source path tracked for the current session. Set by
  // "Import IFC" after the user selects a file via the picker. All
  // subsequent BIM ops (export, validate, schedule, diff) operate
  // on this path. Null when no IFC has been imported yet.
  const [ifcSourcePath, setIfcSourcePath] = useState<string | null>(null);

  const projectPath = project?.path ?? null;

  // Defense-in-depth against project switches racing in-flight
  // `importIfc` / `attachIfc` / `validate` / `classify` /
  // `generateSchedule` / `diff` / `boq` bridge calls and the
  // dialog round-trips that precede them. Each branch captures
  // `startPath` at handler entry and re-checks
  // `getActiveProjectPath()` after every await to detect a project
  // transition mid-flight, then skips stale `setState` / toast
  // commits when the comparison fails.
  //
  //   * Today the `RequireProject` route guard unmounts Bim on
  //     every project transition, so a stale `setIfcSourcePath` /
  //     `setFindings` / `setSchedules` would be a no-op on a
  //     torn-down component. The page is safe in production.
  //   * BUT the route-guard umbrella is the same brittle contract
  //     the per-project reset effect (lines 122-129) chose not to
  //     rely on. Future in-page project pickers, "switch to
  //     recent" toolbar actions, or any code path that calls
  //     `openProject(...)` without forcing a route change would
  //     let project A's import result land into project B's
  //     state — worse, the file dialog's resolved path is for
  //     project A's IFC but `ifcSourcePath` would be associated
  //     with project B's session, then validate/schedule against
  //     it would silently target the wrong project.
  //
  // The page used to mirror `project?.path` into a local
  // `useRef + useEffect`; that pattern had a one-render-cycle lag
  // versus `useActiveProject`'s internal synchronous ref (see the
  // `getActiveProjectPath` docblock on `ActiveProjectState` for
  // the full timing analysis), so a bridge promise resolving in
  // the microtask between a project-switch's sync `updateProject`
  // and this page's `useEffect` re-sync would see a stale local
  // ref and let project A's result commit onto project B. Reading
  // through `getActiveProjectPath()` on every guard site routes
  // the check through the central sync ref so every consumer reads
  // the same source of truth at the same moment — matching the
  // pattern landed in `Deliver.tsx`, `Render.tsx`, and the
  // internal guard in `useActiveProject.saveProject`.

  // Reset every per-project piece of BIM state whenever the active
  // project changes (or closes). Today this is defence in depth —
  // every project-switch path navigates away from `/bim` and the
  // `RequireProject` route guard unmounts the page, so `useState`
  // is reset for free. But the moment a future feature reuses the
  // mounted Bim instance across projects (in-page project picker,
  // a `useEffect` that calls `openProject` without navigating,
  // multi-pane layout, etc.) the page would silently leak
  // project A's IFC into project B: validate/classify/schedule
  // would all run against the stale `ifcSourcePath` while the
  // user sees project B's metadata. The same risk applies to the
  // spatial tree (`root`/`selectedId`), Pset edits (`psets`), and
  // generated schedule rows / validation findings. Mirroring the
  // pattern used by `Render.tsx` (which resets `cameras` /
  // `selectedCameras` on `project?.path` change) — see that
  // useEffect for the long-form rationale — keeps every mode
  // page consistent in how it handles project transitions.
  useEffect(() => {
    setIfcSourcePath(null);
    setRoot(DEMO_ROOT);
    setSelectedId(null);
    setPsets(DEMO_PSETS);
    setSchedules({});
    setFindings([]);
  }, [projectPath]);

  const onInvoke = async (action: BimAction) => {
    // Capture the active project at handler entry so every
    // setState / success-toast site below can check
    // `getActiveProjectPath() !== startPath` and skip stale
    // commits when the user project-switched while the bridge
    // call (or file dialog) was in flight. See the comment on
    // the per-page defense-in-depth pattern above for the long-
    // form rationale. Errors surfaced through the outer catch
    // fire unconditionally (project-agnostic UX: "your last
    // action failed" is useful even after a project switch),
    // matching the `Deliver.tsx onBuildPack` / `onCompare`
    // convention.
    //
    // Read the start path through `getActiveProjectPath()` rather
    // than `project?.path` (the render-time closure value) so the
    // capture sees the freshest sync-ref value — when a project
    // transition lands in the same event tick as the handler
    // dispatch (e.g. a click handler that opens project B *before*
    // dispatching this onInvoke synchronously), the ref already
    // reflects B while the closure still sees A. This keeps the
    // capture and the post-await checks reading from the same
    // source.
    const startPath = getActiveProjectPath();
    setBusyAction(action);
    try {
      switch (action) {
        case "importIfc": {
          const dialog = await aec.dialog.openFile({
            title: "Import IFC",
            filters: IFC_FILTERS,
          });
          if (dialog.canceled || dialog.paths.length === 0) break;
          // If the user project-switched while the file dialog
          // was open, the picked path was chosen for project A
          // but the active project is now B. Importing it into
          // B's snapshot cache would land a project-A IFC into
          // project B's state — worse, subsequent
          // validate/schedule actions would target the wrong
          // project. Abort the import entirely; the user can
          // re-trigger from the new project.
          if (getActiveProjectPath() !== startPath) break;
          const selectedPath = dialog.paths[0];
          const outcome = await importIfcWithSizeGuard(selectedPath);
          // Skip stale: don't commit project A's import result
          // (or its success toast) onto project B's state.
          if (getActiveProjectPath() !== startPath) break;
          if (outcome.kind === "imported") {
            setIfcSourcePath(selectedPath);
            if (outcome.result.spatialNodes > 0 || outcome.result.elements > 0) {
              setRoot(DEMO_ROOT);
            }
            addToast(
              "success",
              `Imported ${outcome.result.elements} elements from IFC`,
            );
          } else if (outcome.kind === "cancelled-by-user") {
            addToast("info", "IFC import cancelled");
          } else {
            addToast("error", `IFC import failed: ${outcome.error}`);
          }
          break;
        }
        case "attachIfc": {
          if (!startPath) {
            addToast("error", "No project open — open or create one first");
            break;
          }
          let attachPath = ifcSourcePath;
          if (!attachPath) {
            const dialog = await aec.dialog.openFile({
              title: "Select IFC to attach",
              filters: IFC_FILTERS,
            });
            if (dialog.canceled || dialog.paths.length === 0) break;
            // Picked path was chosen for project A; if the user
            // switched mid-dialog, attaching it to the (now
            // different) `startPath` captured at entry would
            // bind project A's IFC to project B's DB. Abort.
            if (getActiveProjectPath() !== startPath) break;
            attachPath = dialog.paths[0];
          }
          // Bind the IFC to the project that was active when the
          // handler dispatched (the `startPath` capture), not the
          // closure-captured `project?.path` from render time.
          // `getActiveProjectPath()` reads through `projectPathRef`
          // which `updateProject` writes synchronously, while
          // `project` updates through React's batched commit phase.
          // The two can diverge inside the microsecond window
          // between a synchronous `updateProject(B)` call and
          // React committing the re-render — if a click handler
          // fires `onInvoke` in that window, `startPath` reflects
          // B while the closure `projectPath` still reflects A.
          // The post-await guard `getActiveProjectPath() !== startPath`
          // would pass (B === B) while a closure-captured
          // `attachIfcToProject(projectPath, ...)` call would address
          // A — silently binding project A's IFC to project B's DB.
          // Reading from `startPath` closes that theoretical gap and
          // keeps every bridge-call argument inside this handler
          // sourced from the same ref the post-await guards read.
          const attachResult = await attachIfcToProject(
            startPath,
            attachPath,
          );
          // Skip stale: don't land project A's attach result
          // (or its toast) onto project B's UI.
          if (getActiveProjectPath() !== startPath) break;
          if (attachResult.kind === "attached") {
            setIfcSourcePath(attachPath);
            addToast(
              "success",
              `Attached IFC: ${attachResult.result.elementsInserted} new, ` +
                `${attachResult.result.elementsUpdated} updated`,
            );
          } else {
            addToast("error", `Attach failed: ${attachResult.error}`);
          }
          break;
        }
        case "exportIfc": {
          if (!ifcSourcePath) {
            addToast("error", "Import an IFC first before exporting");
            break;
          }
          // Source the default-path prefix from `startPath` (the
          // sync-ref capture at handler entry) rather than the
          // closure-captured `project?.path`. See the long-form
          // rationale on the `attachIfc` bridge-call argument
          // above — same race window, same correctness argument:
          // the dialog-default-path the user sees must match the
          // project the post-await guards check against.
          const saveResult = await aec.dialog.saveFile({
            title: "Export IFC",
            defaultPath: startPath
              ? `${startPath}/export.ifc`
              : "export.ifc",
            filters: IFC_FILTERS,
          });
          if (saveResult.canceled || !saveResult.path) break;
          // If the user project-switched while the save dialog
          // was open, the chosen output path was picked for
          // project A's export, but `ifcSourcePath` was reset to
          // `null` by the per-project reset effect when the
          // switch fired — so the `aec.bim.exportIfc` call
          // below would either fail with an empty-source error
          // (assertString guard) or, if the ref hasn't been
          // updated by React yet, write project A's IFC to a
          // path the user chose under project A's mental model.
          // Abort to avoid both failure modes.
          if (getActiveProjectPath() !== startPath) break;
          await aec.bim.exportIfc({
            sourcePath: ifcSourcePath,
            outPath: saveResult.path,
          });
          // Skip stale success toast: announcing a project-A
          // export on project B's UI would mislead the user.
          // The file itself was still written to the user-chosen
          // path; this just declines to announce it.
          if (getActiveProjectPath() !== startPath) break;
          addToast("success", `IFC exported to ${saveResult.path}`);
          break;
        }
        case "validate": {
          if (!ifcSourcePath) {
            addToast("error", "Import an IFC first before validating");
            break;
          }
          const result = await aec.bim.validate({
            sourcePath: ifcSourcePath,
          });
          // Skip stale: project A's findings must not land into
          // project B's panel — the per-project reset effect
          // already cleared `findings` to `[]` on the switch.
          if (getActiveProjectPath() !== startPath) break;
          setFindings(bimReportToFindings(result));
          addToast(
            result.ok ? "success" : "info",
            `Validation: ${result.errors.length} errors, ${result.warnings.length} warnings`,
          );
          break;
        }
        case "classify": {
          // `bim_classify` operates on the active project's entity
          // graph (not a single entity) and the Rust bridge requires
          // both `projectPath` and `scheme`. The previous call shape
          // (`{ entityId, source: "ai" }`) would have failed at
          // `requireProjectPath` / `requireStringField("scheme")` in
          // `bridge.ts` (the in-process fallback hid the regression
          // by ignoring unknown fields). We default to the IFC
          // scheme — the most useful auto-classify that maps
          // entity kinds to their canonical IFC classes
          // (`IfcWall`, `IfcDoor`, …). Future iterations can let
          // the user choose between `ifc` / `uniformat-ii` /
          // `omniclass-21` via a small picker; surfacing the
          // counts in a toast keeps the operation observable
          // without that UI.
          if (!startPath) {
            addToast("error", "No project open — open or create one first");
            break;
          }
          // Address `classify` at the project that was active when
          // the handler dispatched. See the long-form rationale on
          // the `attachIfc` bridge-call argument above.
          const result = await aec.bim.classify({
            projectPath: startPath,
            scheme: "ifc",
          });
          // Skip stale: announcing project A's classify counts on
          // project B's UI would mislead the user about which
          // graph the counts apply to. The classify itself ran
          // against project A's DB (bridge holds the write
          // lock); this just declines to toast it.
          if (getActiveProjectPath() !== startPath) break;
          addToast(
            "success",
            `Classified ${result.classified} entit${result.classified === 1 ? "y" : "ies"}` +
              ` (${result.unchanged} unchanged, ${result.skipped} skipped)`,
          );
          break;
        }
        case "generateSchedule": {
          if (!ifcSourcePath) {
            addToast("error", "Import an IFC first before generating schedules");
            break;
          }
          // Build the output path against the project that was
          // active when the handler dispatched (the `startPath`
          // sync-ref capture), not the closure-captured
          // `project?.path`. See the long-form rationale on the
          // `attachIfc` bridge-call argument above for the timing
          // analysis — the same microsecond window between
          // synchronous `updateProject(B)` and React's batched
          // commit applies here. Writing the XLSX into project A's
          // schedules folder while `startPath` already addresses
          // project B would leave an orphaned file under A's tree
          // and announce success under B's UI — both ends wrong.
          const outPath = startPath
            ? `${startPath}/schedules/rooms.xlsx`
            : "rooms.xlsx";
          const summary = await aec.bim.generateSchedule({
            sourcePath: ifcSourcePath,
            outPath,
            kind: "room",
          });
          // Skip stale: project A's generated XLSX rows must not
          // land into project B's schedule preview. The file was
          // written to disk; this just declines to read it back
          // into UI state. The per-project reset effect already
          // cleared `schedules` to `{}` on the switch.
          if (getActiveProjectPath() !== startPath) break;
          // Read rows back from the generated file.
          let rows: ScheduleRow[] = [];
          if (summary.rows > 0) {
            try {
              const readback = await aec.bim.readScheduleRows({
                xlsxPath: summary.outPath,
              });
              rows = readback.rows as ScheduleRow[];
            } catch {
              // Readback failed; display empty rows.
            }
          }
          // Re-check after the optional readback await.
          if (getActiveProjectPath() !== startPath) break;
          setSchedules((prev) => ({ ...prev, room: rows }));
          addToast(
            "success",
            `Generated room schedule: ${summary.rows} rows`,
          );
          break;
        }
        case "diff": {
          const beforeDialog = await aec.dialog.openFile({
            title: "Select 'before' IFC snapshot",
            filters: IFC_FILTERS,
          });
          if (beforeDialog.canceled || beforeDialog.paths.length === 0) break;
          // If the user switched projects between the two
          // dialogs (or between the second dialog and the bridge
          // call), the two picked snapshots were chosen under
          // project A's mental model but the diff result will be
          // announced on project B — abort each step that
          // could land a stale toast on the wrong project's UI.
          if (getActiveProjectPath() !== startPath) break;
          const afterDialog = await aec.dialog.openFile({
            title: "Select 'after' IFC snapshot",
            filters: IFC_FILTERS,
          });
          if (afterDialog.canceled || afterDialog.paths.length === 0) break;
          if (getActiveProjectPath() !== startPath) break;
          const diffResult = await aec.bim.diff({
            beforePath: beforeDialog.paths[0],
            afterPath: afterDialog.paths[0],
          });
          // Skip stale success toast.
          if (getActiveProjectPath() !== startPath) break;
          addToast(
            "info",
            `Diff: ${diffResult.added.length} added, ${diffResult.removed.length} removed, ${diffResult.modified.length} modified`,
          );
          break;
        }
        case "boq": {
          if (!ifcSourcePath) {
            addToast("error", "Import an IFC first");
            break;
          }
          // Same `startPath` sourcing as `generateSchedule`
          // above — keep every bridge-call argument inside this
          // handler sourced from the same ref the post-await guards
          // read. See the long-form rationale on the `attachIfc`
          // branch.
          const boqOut = startPath
            ? `${startPath}/schedules/materials.xlsx`
            : "materials.xlsx";
          const boqSummary = await aec.bim.generateSchedule({
            sourcePath: ifcSourcePath,
            outPath: boqOut,
            kind: "material",
          });
          // Skip stale: same rationale as generateSchedule above.
          if (getActiveProjectPath() !== startPath) break;
          let boqRows: ScheduleRow[] = [];
          if (boqSummary.rows > 0) {
            try {
              const readback = await aec.bim.readScheduleRows({
                xlsxPath: boqSummary.outPath,
              });
              boqRows = readback.rows as ScheduleRow[];
            } catch {
              // Readback failed.
            }
          }
          if (getActiveProjectPath() !== startPath) break;
          setSchedules((prev) => ({ ...prev, material: boqRows }));
          addToast(
            "success",
            `Generated BOQ: ${boqSummary.rows} rows`,
          );
          break;
        }
      }
    } catch (err) {
      // Centralized error surface for every BIM operation.
      // `exportIfc` / `validate` / `classify` / `diff` /
      // `generateSchedule` / `boq` all run against real OS paths
      // from the file picker, so transient failures (disk full,
      // permission denied, malformed IFC, locked SQLCipher DB) are
      // realistic and must reach the user. The single `catch`
      // covers all six branches uniformly and matches the toast
      // convention used by Draft/Deliver/App save — putting
      // per-case `try/catch` blocks inline would duplicate the
      // same toast call six times and let one branch silently
      // diverge from the others in a future patch.
      addToast(
        "error",
        `${action} failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      setBusyAction(null);
    }
  };

  const classification =
    selectedId === "lvl_l1" ? "IfcBuildingStorey" : selectedId ? "IfcWall" : null;

  const scheduleSourcePath = ifcSourcePath ?? "";
  const scheduleOutPathForKind = (kind: ScheduleKind): string =>
    projectPath
      ? `${projectPath}/schedules/${kind}.xlsx`
      : `${kind}.xlsx`;

  // Consume the summary that `ScheduleView.regenerate()` already
  // produced — calling `generateSchedule` a second time would write
  // the XLSX twice with identical content. We only need to read the
  // rows back from the file the bridge just wrote.
  //
  // Per-project guard mirroring the pattern in `onInvoke` above:
  // capture `getActiveProjectPath()` at handler entry and re-check
  // after every await. If the user project-switched while
  // `regenerate()`'s `generateSchedule` bridge call was in flight,
  // `ScheduleView.regenerate()` will resolve and dispatch this
  // `onGenerate` callback against project B's hooks; the readback
  // await then races the switch a second time. Without the guard,
  // project A's rows would land into project B's `schedules` state
  // through the route-guard's unmount window (one paint frame
  // today, longer with any future in-page project picker or
  // `openProject` without navigate). Matches the defense-in-depth
  // tier established by every async branch of `onInvoke` so the
  // file presents one consistent shape: every async handler
  // captures `startPath` and checks the ref after each await
  // before committing to React state. RequireProject unmounting
  // makes the race unreachable in production today, but the
  // structural guarantee survives future routing changes.
  const onScheduleGenerate = async (
    kind: ScheduleKind,
    summary: { outPath: string; rows: number },
  ) => {
    const startPath = getActiveProjectPath();
    let rows: ScheduleRow[] = [];
    if (summary.rows > 0) {
      try {
        const readback = await aec.bim.readScheduleRows({
          xlsxPath: summary.outPath,
        });
        rows = readback.rows as ScheduleRow[];
      } catch {
        // Readback failed — leave the inline preview empty; the
        // XLSX on disk is still authoritative.
      }
    }
    // Skip the state commit if a project transition (open /
    // create / close / switch) ran while `regenerate()` or the
    // readback was in flight. The XLSX on disk still belongs to
    // project A — landing its rows in project B's `schedules`
    // would silently leak project A's data into project B's
    // schedule preview. The bridge call itself is not wasted:
    // the file was written successfully under project A, and the
    // user can re-open project A to see it.
    if (getActiveProjectPath() !== startPath) return;
    setSchedules((prev) => ({ ...prev, [kind]: rows }));
  };

  return (
    <div className="bim-layout" data-testid="bim-mode">
      <BimToolbar busyAction={busyAction} onInvoke={onInvoke} />
      <div className="bim-center">
        <SpatialTree
          root={root}
          selectedId={selectedId}
          onSelect={setSelectedId}
        />
        <div
          className="bim-viewport"
          aria-label="3D viewport"
          data-testid="bim-viewport"
        >
          {selectedId ? (
            <p>Showing: {selectedId}</p>
          ) : (
            <p>Select an element from the spatial tree.</p>
          )}
        </div>
        <PropertyEditor
          entityId={selectedId}
          classification={classification}
          psets={psets}
          onChange={setPsets}
        />
      </div>
      <div className="bim-bottom">
        <ScheduleView
          sourcePath={scheduleSourcePath}
          outPathForKind={scheduleOutPathForKind}
          rowsByKind={schedules}
          onGenerate={(kind, summary) => {
            void onScheduleGenerate(kind, summary);
          }}
          // Route bridge failures from `ScheduleView.regenerate` into
          // the same toast system used by the toolbar's centralized
          // `onInvoke` catch (line ~328). The bridge runs against
          // real OS paths from the file picker, so disk-full /
          // permission-denied / locked-DB errors are real failure
          // surfaces and need to reach the user. The panel handles
          // the catch internally and forwards a pre-formatted
          // message here so this page doesn't need to know about
          // the bridge's specific error shapes.
          onError={(msg) => addToast("error", msg)}
        />
        <ValidatorPanel
          sourcePath={ifcSourcePath ?? ""}
          findings={findings}
          onFindings={setFindings}
          onZoomTo={(id) => setSelectedId(id)}
          // Same rationale as `ScheduleView.onError` above: route
          // bridge failures from `ValidatorPanel.revalidate` into
          // the toast system so file-moved / permission-denied /
          // malformed-IFC errors surface visibly instead of becoming
          // unhandled promise rejections from `onClick`.
          onError={(msg) => addToast("error", msg)}
        />
      </div>
    </div>
  );
}
