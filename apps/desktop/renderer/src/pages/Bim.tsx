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
import {
  ValidatorPanel,
  ValidationFinding,
} from "../components/bim/ValidatorPanel";
import {
  BimToolbar,
  BimAction,
} from "../components/bim/BimToolbar";

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

export function Bim() {
  const [root, setRoot] = useState<SpatialNode | null>(DEMO_ROOT);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [psets, setPsets] = useState<PsetData>(DEMO_PSETS);
  const [schedules, setSchedules] = useState<
    Partial<Record<ScheduleKind, ScheduleRow[]>>
  >({});
  const [findings, setFindings] = useState<ValidationFinding[]>([]);
  const [busyAction, setBusyAction] = useState<BimAction | null>(null);

  const onInvoke = async (action: BimAction) => {
    setBusyAction(action);
    try {
      switch (action) {
        case "importIfc": {
          // Run through the size-guarded helper so a multi-hundred-MB
          // IFC file surfaces a confirm dialog *before* the bridge
          // commits to the (several-seconds) STEP-21 parse. See
          // `bim-import.ts` for the contract.
          const outcome = await importIfcWithSizeGuard(
            "demo://project.ifc",
          );
          if (outcome.kind === "imported") {
            // PR-P wired `bim_import_ifc` to the native bridge, so
            // `outcome.result` is now a full `BimImportSummary`
            // with entity counts. If the bridge actually parsed
            // anything (i.e. we're running against a real file via
            // the native path, not the all-zeros in-process
            // fallback), refresh the demo tree. A future PR will
            // replace the demo tree with a real one folded from
            // the snapshot.
            if (outcome.result.spatialNodes > 0 || outcome.result.elements > 0) {
              setRoot(DEMO_ROOT);
            }
          }
          // "cancelled-by-user" / "failed" outcomes are no-ops on
          // the demo tree — a future PR will surface them as a
          // toast or status-pane note.
          break;
        }
        case "attachIfc": {
          // Fold a previously-parsed IFC snapshot into the active
          // project's SQLCipher DB. The bridge's in-process
          // snapshot cache means an import → attach handoff on
          // the same path does NOT re-parse the file
          // (`parseCacheHit: true` in the result).
          //
          // The demo uses placeholder paths because the page-level
          // project-picker UX isn't wired in this PR; a future PR
          // will replace these with the currently-open project's
          // path and a file-picker-supplied IFC path. The fallback
          // backend will return all-zero counts here, so the demo
          // tree stays untouched.
          await attachIfcToProject(
            "demo://project.aecstudio",
            "demo://project.ifc",
          );
          break;
        }
        case "exportIfc":
          // PR-T wired `bim_export_ifc` to the native bridge. The
          // bridge re-serialises the parsed snapshot back to
          // STEP-21 and writes it to `outPath`. Demo paths until a
          // file-picker UX lands.
          await aec.bim.exportIfc({
            sourcePath: "demo://project.ifc",
            outPath: "demo://project.out.ifc",
          });
          break;
        case "validate": {
          // PR-T wired `bim_validate` to the native bridge. The
          // bridge runs the rule-based BIM validator over the
          // parsed snapshot and splits findings into three vectors
          // (errors / warnings / infos). The wire format uses
          // `description` / `element` / `suggestion`; the renderer
          // re-maps those to the existing `message` / `entityId` /
          // `hint` shape used by `ValidatorPanel`.
          const result = await aec.bim.validate({
            sourcePath: "demo://project.ifc",
          });
          const merged: ValidationFinding[] = [
            ...result.errors.map((f) => ({
              code: f.code,
              severity: "error" as const,
              message: f.description,
              entityId: f.element,
              hint: f.suggestion,
            })),
            ...result.warnings.map((f) => ({
              code: f.code,
              severity: "warning" as const,
              message: f.description,
              entityId: f.element,
              hint: f.suggestion,
            })),
            ...result.infos.map((f) => ({
              code: f.code,
              severity: "info" as const,
              message: f.description,
              entityId: f.element,
              hint: f.suggestion,
            })),
          ];
          setFindings(merged);
          break;
        }
        case "classify":
          await aec.bim.classify({ entityId: selectedId, source: "ai" });
          break;
        case "generateSchedule": {
          // PR-T wired `bim_generate_schedule` to the native
          // bridge. The bridge writes the XLSX directly to disk,
          // returning row/column counts but NOT the row data — so
          // `setSchedules` clears any stale rows until a future PR
          // adds an XLSX-to-row-list parse step.
          await aec.bim.generateSchedule({
            sourcePath: "demo://project.ifc",
            outPath: "demo://project.rooms.xlsx",
            kind: "room",
          });
          setSchedules((prev) => ({ ...prev, room: [] }));
          break;
        }
        case "diff":
          // PR-T wired `bim_diff` to the native bridge. Demo paths
          // until a file-picker UX lands.
          await aec.bim.diff({
            beforePath: "demo://snapshot.a.ifc",
            afterPath: "demo://snapshot.b.ifc",
          });
          break;
        case "boq":
          await aec.bim.generateSchedule({
            sourcePath: "demo://project.ifc",
            outPath: "demo://project.materials.xlsx",
            kind: "material",
          });
          break;
      }
    } finally {
      setBusyAction(null);
    }
  };

  const classification =
    selectedId === "lvl_l1" ? "IfcBuildingStorey" : selectedId ? "IfcWall" : null;

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
          sourcePath="demo://project.ifc"
          outPathForKind={(kind) => `demo://project.${kind}.xlsx`}
          rowsByKind={schedules}
          onGenerate={(kind) =>
            // The native bridge writes the XLSX directly; we don't
            // get rows back, so clear any stale row data.
            setSchedules((prev) => ({ ...prev, [kind]: [] }))
          }
        />
        <ValidatorPanel
          sourcePath="demo://project.ifc"
          findings={findings}
          onFindings={setFindings}
          onZoomTo={(id) => setSelectedId(id)}
        />
      </div>
    </div>
  );
}
