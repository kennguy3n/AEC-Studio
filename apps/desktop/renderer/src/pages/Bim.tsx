import { useEffect, useRef, useState } from "react";
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
  const { project } = useActiveProject();
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

  // Synchronous mirror of `projectPath` so `onInvoke` can check
  // "is the project I started on still the active one?" after each
  // await. Defense-in-depth against project switches racing in-
  // flight `importIfc` / `attachIfc` / `validate` / `classify` /
  // `generateSchedule` / `diff` / `boq` bridge calls and the
  // dialog round-trips that precede them.
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
  //   * Capturing `projectPath` at the start of `onInvoke` and
  //     comparing against `projectPathRef.current` after each
  //     await closes that gap by structural construction. Matches
  //     the per-call-site guard pattern in `Deliver.tsx` (which
  //     uses an identical `projectPathRef` for `onCompare` /
  //     `onCreateRevision` / `onBuildPack`) and the internal
  //     `projectPathRef` in `useActiveProject.saveProject`.
  //
  // Synced via `useEffect` keyed on `projectPath` so the ref
  // tracks the latest path at microtask boundary — the
  // `onInvoke` invocation that captured the old path will still
  // see its captured value, and the ref comparison correctly
  // identifies the project switch.
  const projectPathRef = useRef<string | null>(projectPath);
  useEffect(() => {
    projectPathRef.current = projectPath;
  }, [projectPath]);

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
    // `projectPathRef.current !== startPath` and skip stale
    // commits when the user project-switched while the bridge
    // call (or file dialog) was in flight. See the comment on
    // `projectPathRef` above for the long-form rationale. Errors
    // surfaced through the outer catch fire unconditionally
    // (project-agnostic UX: "your last action failed" is useful
    // even after a project switch), matching the
    // `Deliver.tsx onBuildPack` / `onCompare` convention.
    const startPath = projectPath;
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
          if (projectPathRef.current !== startPath) break;
          const selectedPath = dialog.paths[0];
          const outcome = await importIfcWithSizeGuard(selectedPath);
          // Skip stale: don't commit project A's import result
          // (or its success toast) onto project B's state.
          if (projectPathRef.current !== startPath) break;
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
          if (!projectPath) {
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
            // different) `projectPath` captured at entry would
            // bind project A's IFC to project B's DB. Abort.
            if (projectPathRef.current !== startPath) break;
            attachPath = dialog.paths[0];
          }
          const attachResult = await attachIfcToProject(
            projectPath,
            attachPath,
          );
          // Skip stale: don't land project A's attach result
          // (or its toast) onto project B's UI.
          if (projectPathRef.current !== startPath) break;
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
          const saveResult = await aec.dialog.saveFile({
            title: "Export IFC",
            defaultPath: projectPath
              ? `${projectPath}/export.ifc`
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
          if (projectPathRef.current !== startPath) break;
          await aec.bim.exportIfc({
            sourcePath: ifcSourcePath,
            outPath: saveResult.path,
          });
          // Skip stale success toast: announcing a project-A
          // export on project B's UI would mislead the user.
          // The file itself was still written to the user-chosen
          // path; this just declines to announce it.
          if (projectPathRef.current !== startPath) break;
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
          if (projectPathRef.current !== startPath) break;
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
          if (!projectPath) {
            addToast("error", "No project open — open or create one first");
            break;
          }
          const result = await aec.bim.classify({
            projectPath,
            scheme: "ifc",
          });
          // Skip stale: announcing project A's classify counts on
          // project B's UI would mislead the user about which
          // graph the counts apply to. The classify itself ran
          // against project A's DB (bridge holds the write
          // lock); this just declines to toast it.
          if (projectPathRef.current !== startPath) break;
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
          const outPath = projectPath
            ? `${projectPath}/schedules/rooms.xlsx`
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
          if (projectPathRef.current !== startPath) break;
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
          if (projectPathRef.current !== startPath) break;
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
          if (projectPathRef.current !== startPath) break;
          const afterDialog = await aec.dialog.openFile({
            title: "Select 'after' IFC snapshot",
            filters: IFC_FILTERS,
          });
          if (afterDialog.canceled || afterDialog.paths.length === 0) break;
          if (projectPathRef.current !== startPath) break;
          const diffResult = await aec.bim.diff({
            beforePath: beforeDialog.paths[0],
            afterPath: afterDialog.paths[0],
          });
          // Skip stale success toast.
          if (projectPathRef.current !== startPath) break;
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
          const boqOut = projectPath
            ? `${projectPath}/schedules/materials.xlsx`
            : "materials.xlsx";
          const boqSummary = await aec.bim.generateSchedule({
            sourcePath: ifcSourcePath,
            outPath: boqOut,
            kind: "material",
          });
          // Skip stale: same rationale as generateSchedule above.
          if (projectPathRef.current !== startPath) break;
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
          if (projectPathRef.current !== startPath) break;
          setSchedules((prev) => ({ ...prev, material: boqRows }));
          addToast(
            "success",
            `Generated BOQ: ${boqSummary.rows} rows`,
          );
          break;
        }
      }
    } catch (err) {
      // Centralized error surface for every BIM operation. Before
      // Phase 13 these branches all targeted `demo://...` paths
      // resolved by the in-process fallback (which never throws), so
      // a missing catch was benign. Now that `exportIfc` / `validate`
      // / `classify` / `diff` / `generateSchedule` / `boq` all run
      // against real OS paths from the file picker, transient
      // failures (disk full, permission denied, malformed IFC,
      // locked SQLCipher DB) are realistic and must reach the user.
      // The single `catch` covers all six branches uniformly and
      // matches the toast convention used by Draft/Deliver/App save
      // — putting per-case `try/catch` blocks inline would duplicate
      // the same toast call six times and let one branch silently
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
  const onScheduleGenerate = async (
    kind: ScheduleKind,
    summary: { outPath: string; rows: number },
  ) => {
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
          // `onInvoke` catch (line ~328). Pre-Phase 13 every branch
          // hit `demo://...` paths (in-process fallback never threw)
          // so the missing catch was benign; Phase 13 wires real OS
          // paths from the file picker, so disk-full / permission
          // denied / locked-DB errors are now real failure surfaces.
          // The panel handles the catch internally and forwards a
          // pre-formatted message here so this page doesn't need to
          // know about the bridge's specific error shapes.
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
