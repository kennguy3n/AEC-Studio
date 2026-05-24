import { contextBridge, ipcRenderer } from "electron";

/**
 * Typed IPC bridge that the renderer talks to. Every channel here mirrors
 * `ARCHITECTURE.md → TypeScript API interfaces`. The Rust N-API bridge
 * implements the real backing; in the renderer we just call `invoke`.
 */
const api = {
  // ----- Project -----
  project: {
    createFromTemplate: (templateKey: string, projectName: string) =>
      ipcRenderer.invoke("project:createFromTemplate", { templateKey, projectName }),
    open: (projectPath: string) => ipcRenderer.invoke("project:open", { projectPath }),
    save: (projectPath: string) => ipcRenderer.invoke("project:save", { projectPath }),
    listRecents: () => ipcRenderer.invoke("project:listRecents"),
    exportPackage: (projectPath: string, outPath: string) =>
      ipcRenderer.invoke("project:exportPackage", { projectPath, outPath }),
  },

  // ----- Design -----
  design: {
    placeFurniture: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("design:placeFurniture", params),
    paintMaterial: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("design:paintMaterial", params),
    setLighting: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("design:setLighting", params),
    saveCamera: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("design:saveCamera", params),
    listAssets: (query: Record<string, unknown>) =>
      ipcRenderer.invoke("design:listAssets", query),
  },

  // ----- Draft -----
  draft: {
    drawPrimitive: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("draft:drawPrimitive", params),
    editTool: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("draft:editTool", params),
    createSheet: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("draft:createSheet", params),
    setLayerState: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("draft:setLayerState", params),
    importDxf: (path: string) => ipcRenderer.invoke("draft:importDxf", { path }),
    exportDxf: (path: string) => ipcRenderer.invoke("draft:exportDxf", { path }),
  },

  // ----- BIM -----
  bim: {
    importIfc: (path: string) => ipcRenderer.invoke("bim:importIfc", { path }),
    /**
     * Cheap pre-parse file-size check. The renderer's file-picker UI
     * calls this before `importIfc` so it can warn-and-confirm on
     * files at or above the 100 MB threshold without first paying
     * the multi-second parse cost.
     */
    checkFileSize: (path: string) =>
      ipcRenderer.invoke("bim:checkFileSize", { path }) as Promise<{
        path: string;
        fileSizeBytes: number;
        largeFileWarning: boolean;
        thresholdBytes: number;
      }>,
    exportIfc: (path: string) => ipcRenderer.invoke("bim:exportIfc", { path }),
    classify: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("bim:classify", params),
    setProperty: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("bim:setProperty", params),
    generateSchedule: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("bim:generateSchedule", params),
    validate: () => ipcRenderer.invoke("bim:validate"),
    diff: (params: Record<string, unknown>) => ipcRenderer.invoke("bim:diff", params),
  },

  // ----- Render -----
  render: {
    enqueueRender: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("render:enqueueRender", params),
    listJobs: () => ipcRenderer.invoke("render:listJobs"),
    cancelJob: (jobId: string) => ipcRenderer.invoke("render:cancelJob", { jobId }),
    applyPreset: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("render:applyPreset", params),
    diagnose: (jobId: string) => ipcRenderer.invoke("render:diagnose", { jobId }),
    enqueueBatch: (params: {
      cameraIds: string[];
      presetIds?: string[];
      presetId?: string;
    }) => ipcRenderer.invoke("render:enqueueBatch", params),
    batchProgress: (batchId: string) =>
      ipcRenderer.invoke("render:batchProgress", { batchId }),
    checkMaterials: () => ipcRenderer.invoke("render:checkMaterials"),
  },

  // ----- AI -----
  ai: {
    listTools: () => ipcRenderer.invoke("ai:listTools"),
    plan: (params: Record<string, unknown>) => ipcRenderer.invoke("ai:plan", params),
    acceptDiff: (diffId: string) => ipcRenderer.invoke("ai:acceptDiff", { diffId }),
    rejectDiff: (diffId: string) => ipcRenderer.invoke("ai:rejectDiff", { diffId }),
    cancelJob: (jobId: string) => ipcRenderer.invoke("ai:cancelJob", { jobId }),
    runtimeStatus: () => ipcRenderer.invoke("ai:runtimeStatus"),
  },

  // ----- Export -----
  export: {
    exportPdf: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("export:exportPdf", params),
    exportDxf: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("export:exportDxf", params),
    exportIfc: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("export:exportIfc", params),
    exportGltf: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("export:exportGltf", params),
    buildProposalPack: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("export:buildProposalPack", params),
  },

  // ----- Deliver -----
  deliver: {
    createRevision: (params: {
      tag: string;
      description: string;
      entities?: Array<{
        category: string;
        id: string;
        payloadHash: string;
        label?: string | null;
      }>;
    }) => ipcRenderer.invoke("deliver:createRevision", params),
    listRevisions: () => ipcRenderer.invoke("deliver:listRevisions"),
    compareRevisions: (params: { baseId: string; headId: string }) =>
      ipcRenderer.invoke("deliver:compareRevisions", params),
    buildPack: (params: {
      kind: "concept" | "interior" | "contractor" | "bim";
      outPath: string;
      includeRenders?: boolean;
      includeSheets?: boolean;
      includeIfc?: boolean;
      includeBoq?: boolean;
      includeProposal?: boolean;
      region?: "eu" | "na" | "apac";
    }) => ipcRenderer.invoke("deliver:buildPack", params),
  },

  // ----- Runtime -----
  runtime: {
    status: () => ipcRenderer.invoke("runtime:status"),
  },
};

contextBridge.exposeInMainWorld("aec", api);

export type AecApi = typeof api;
