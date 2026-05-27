import { useState } from "react";
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

  const onInvoke = async (action: BimAction) => {
    setBusyAction(action);
    try {
      switch (action) {
        case "importIfc": {
          const dialog = await aec.dialog.openFile({
            title: "Import IFC",
            filters: IFC_FILTERS,
          });
          if (dialog.canceled || dialog.paths.length === 0) break;
          const selectedPath = dialog.paths[0];
          const outcome = await importIfcWithSizeGuard(selectedPath);
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
            attachPath = dialog.paths[0];
          }
          const attachResult = await attachIfcToProject(
            projectPath,
            attachPath,
          );
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
          await aec.bim.exportIfc({
            sourcePath: ifcSourcePath,
            outPath: saveResult.path,
          });
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
          const afterDialog = await aec.dialog.openFile({
            title: "Select 'after' IFC snapshot",
            filters: IFC_FILTERS,
          });
          if (afterDialog.canceled || afterDialog.paths.length === 0) break;
          const diffResult = await aec.bim.diff({
            beforePath: beforeDialog.paths[0],
            afterPath: afterDialog.paths[0],
          });
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
          setSchedules((prev) => ({ ...prev, material: boqRows }));
          addToast(
            "success",
            `Generated BOQ: ${boqSummary.rows} rows`,
          );
          break;
        }
      }
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
        />
        <ValidatorPanel
          sourcePath={ifcSourcePath ?? ""}
          findings={findings}
          onFindings={setFindings}
          onZoomTo={(id) => setSelectedId(id)}
        />
      </div>
    </div>
  );
}
