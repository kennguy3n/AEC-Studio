import { ipcMain } from "electron";
import { getBridge } from "./bridge";

/**
 * Register every IPC handler the preload bridge expects. Handlers are
 * grouped by API surface (Project / Design / Draft / BIM / Render / AI /
 * Export / Runtime).
 *
 * Each handler:
 *   1. Validates the input shape (throws a typed error if not).
 *   2. Calls the N-API bridge (`aec_bridge.node`).
 *   3. Returns plain JSON-serialisable values back to the renderer.
 *
 * The renderer never sees the bridge directly — only this layer. Same
 * for any disk paths or process handles.
 */
export function registerIpcHandlers(): void {
  // ----- Project -----
  ipcMain.handle("project:createFromTemplate", async (_e, { templateKey, projectName }) => {
    assertString(templateKey, "templateKey");
    assertString(projectName, "projectName");
    return getBridge().projectCreateFromTemplate(templateKey, projectName);
  });
  ipcMain.handle("project:open", async (_e, { projectPath }) => {
    assertString(projectPath, "projectPath");
    return getBridge().projectOpen(projectPath);
  });
  ipcMain.handle("project:save", async (_e, { projectPath }) => {
    assertString(projectPath, "projectPath");
    return getBridge().projectSave(projectPath);
  });
  ipcMain.handle("project:listRecents", async () => {
    return getBridge().projectListRecents();
  });
  ipcMain.handle("project:exportPackage", async (_e, { projectPath, outPath }) => {
    assertString(projectPath, "projectPath");
    assertString(outPath, "outPath");
    return getBridge().projectExportPackage(projectPath, outPath);
  });

  // ----- Design -----
  ipcMain.handle("design:placeFurniture", async (_e, params) => {
    assertObject(params, "params");
    return getBridge().designPlaceFurniture(params);
  });
  ipcMain.handle("design:paintMaterial", async (_e, params) => {
    assertObject(params, "params");
    return getBridge().designPaintMaterial(params);
  });
  ipcMain.handle("design:setLighting", async (_e, params) => {
    assertObject(params, "params");
    return getBridge().designSetLighting(params);
  });
  ipcMain.handle("design:saveCamera", async (_e, params) => {
    assertObject(params, "params");
    return getBridge().designSaveCamera(params);
  });
  ipcMain.handle("design:listAssets", async (_e, query) => {
    assertObject(query, "query");
    return getBridge().designListAssets(query);
  });

  // ----- Draft -----
  // Same object-shape validation as the Design handlers above. We don't
  // want one renderer-side bug to send `undefined` / a number across the
  // IPC boundary into the native bridge.
  ipcMain.handle("draft:drawPrimitive", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftDrawPrimitive(p);
  });
  ipcMain.handle("draft:editTool", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftEditTool(p);
  });
  ipcMain.handle("draft:createSheet", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftCreateSheet(p);
  });
  ipcMain.handle("draft:setLayerState", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftSetLayerState(p);
  });
  ipcMain.handle("draft:importDxf", async (_e, { path }) => {
    assertString(path, "path");
    return getBridge().draftImportDxf(path);
  });
  ipcMain.handle("draft:exportDxf", async (_e, { path }) => {
    assertString(path, "path");
    return getBridge().draftExportDxf(path);
  });

  // ----- BIM -----
  ipcMain.handle("bim:importIfc", async (_e, { path }) => {
    assertString(path, "path");
    return getBridge().bimImportIfc(path);
  });
  ipcMain.handle("bim:checkFileSize", async (_e, { path }) => {
    assertString(path, "path");
    return getBridge().bimCheckFileSize(path);
  });
  ipcMain.handle("bim:exportIfc", async (_e, { path }) => {
    assertString(path, "path");
    return getBridge().bimExportIfc(path);
  });
  ipcMain.handle("bim:classify", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().bimClassify(p);
  });
  ipcMain.handle("bim:setProperty", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().bimSetProperty(p);
  });
  ipcMain.handle("bim:generateSchedule", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().bimGenerateSchedule(p);
  });
  ipcMain.handle("bim:validate", async () => getBridge().bimValidate());
  ipcMain.handle("bim:diff", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().bimDiff(p);
  });

  // ----- Render -----
  ipcMain.handle("render:enqueueRender", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().renderEnqueue(p);
  });
  ipcMain.handle("render:listJobs", async () => getBridge().renderListJobs());
  ipcMain.handle("render:cancelJob", async (_e, { jobId }) => {
    assertString(jobId, "jobId");
    return getBridge().renderCancelJob(jobId);
  });
  ipcMain.handle("render:applyPreset", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().renderApplyPreset(p);
  });
  ipcMain.handle("render:diagnose", async (_e, { jobId }) => {
    assertString(jobId, "jobId");
    return getBridge().renderDiagnose(jobId);
  });
  ipcMain.handle("render:enqueueBatch", async (_e, p) => {
    assertObject(p, "params");
    const cameraIds = (p as { cameraIds?: unknown }).cameraIds;
    if (!Array.isArray(cameraIds) || cameraIds.length === 0) {
      throw new Error("renderEnqueueBatch: cameraIds must be a non-empty array");
    }
    if (!cameraIds.every((c) => typeof c === "string")) {
      throw new Error("renderEnqueueBatch: cameraIds must contain strings");
    }
    const presetIds = (p as { presetIds?: unknown }).presetIds;
    if (
      presetIds !== undefined &&
      (!Array.isArray(presetIds) ||
        !presetIds.every((s) => typeof s === "string"))
    ) {
      throw new Error("renderEnqueueBatch: presetIds must be string[]");
    }
    const presetId = (p as { presetId?: unknown }).presetId;
    if (presetId !== undefined && typeof presetId !== "string") {
      throw new Error("renderEnqueueBatch: presetId must be a string");
    }
    return getBridge().renderEnqueueBatch({
      cameraIds: cameraIds as string[],
      presetIds: presetIds as string[] | undefined,
      presetId: presetId as string | undefined,
    });
  });
  ipcMain.handle("render:batchProgress", async (_e, { batchId }) => {
    assertString(batchId, "batchId");
    return getBridge().renderBatchProgress(batchId);
  });
  ipcMain.handle("render:checkMaterials", async () =>
    getBridge().renderCheckMaterials(),
  );

  // ----- AI -----
  ipcMain.handle("ai:listTools", async () => getBridge().aiListTools());
  ipcMain.handle("ai:plan", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().aiPlan(p);
  });
  ipcMain.handle("ai:acceptDiff", async (_e, { diffId }) => {
    assertString(diffId, "diffId");
    return getBridge().aiAcceptDiff(diffId);
  });
  ipcMain.handle("ai:rejectDiff", async (_e, { diffId }) => {
    assertString(diffId, "diffId");
    return getBridge().aiRejectDiff(diffId);
  });
  ipcMain.handle("ai:cancelJob", async (_e, { jobId }) => {
    assertString(jobId, "jobId");
    return getBridge().aiCancelJob(jobId);
  });
  ipcMain.handle("ai:runtimeStatus", async () => getBridge().aiRuntimeStatus());

  // ----- Export -----
  ipcMain.handle("export:exportPdf", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().exportPdf(p);
  });
  ipcMain.handle("export:exportDxf", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().exportDxf(p);
  });
  ipcMain.handle("export:exportIfc", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().exportIfc(p);
  });
  ipcMain.handle("export:exportGltf", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().exportGltf(p);
  });
  ipcMain.handle("export:buildProposalPack", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().exportBuildProposalPack(p);
  });

  // ----- Deliver -----
  ipcMain.handle("deliver:createRevision", async (_e, p) => {
    assertObject(p, "params");
    assertString(p.tag, "tag");
    const description = typeof p.description === "string" ? p.description : "";
    const entities = Array.isArray(p.entities)
      ? (p.entities as Array<Record<string, unknown>>).map((e) => {
          assertString(e.category, "entity.category");
          assertString(e.id, "entity.id");
          assertString(e.payloadHash, "entity.payloadHash");
          return {
            category: e.category as string,
            id: e.id as string,
            payloadHash: e.payloadHash as string,
            label: typeof e.label === "string" ? (e.label as string) : null,
          };
        })
      : undefined;
    return getBridge().deliverCreateRevision({
      tag: p.tag,
      description,
      entities,
    });
  });
  ipcMain.handle("deliver:listRevisions", async () =>
    getBridge().deliverListRevisions(),
  );
  ipcMain.handle("deliver:compareRevisions", async (_e, p) => {
    assertObject(p, "params");
    assertString(p.baseId, "baseId");
    assertString(p.headId, "headId");
    return getBridge().deliverCompareRevisions({
      baseId: p.baseId,
      headId: p.headId,
    });
  });
  ipcMain.handle("deliver:buildPack", async (_e, p) => {
    assertObject(p, "params");
    assertString(p.kind, "kind");
    assertString(p.outPath, "outPath");
    const allowedKinds = new Set(["concept", "interior", "contractor", "bim"]);
    if (!allowedKinds.has(p.kind)) {
      throw new IpcValidationError(
        `kind must be one of concept|interior|contractor|bim (got ${p.kind})`,
      );
    }
    const region =
      typeof p.region === "string" &&
      ["eu", "na", "apac"].includes(p.region as string)
        ? (p.region as "eu" | "na" | "apac")
        : undefined;
    return getBridge().deliverBuildPack({
      kind: p.kind as "concept" | "interior" | "contractor" | "bim",
      outPath: p.outPath,
      includeRenders: typeof p.includeRenders === "boolean" ? p.includeRenders : undefined,
      includeSheets: typeof p.includeSheets === "boolean" ? p.includeSheets : undefined,
      includeIfc: typeof p.includeIfc === "boolean" ? p.includeIfc : undefined,
      includeBoq: typeof p.includeBoq === "boolean" ? p.includeBoq : undefined,
      includeProposal: typeof p.includeProposal === "boolean" ? p.includeProposal : undefined,
      region,
    });
  });

  // ----- Runtime -----
  ipcMain.handle("runtime:status", async () => getBridge().runtimeStatus());
}

// ----- validation helpers (small, real, not stubs) -----

function assertString(value: unknown, field: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new IpcValidationError(`${field} must be a non-empty string`);
  }
}

function assertObject(
  value: unknown,
  field: string,
): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new IpcValidationError(`${field} must be an object`);
  }
}

export class IpcValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "IpcValidationError";
  }
}
