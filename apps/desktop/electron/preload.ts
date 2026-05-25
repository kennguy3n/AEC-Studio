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
    importIfc: (path: string) =>
      ipcRenderer.invoke("bim:importIfc", { path }) as Promise<{
        path: string;
        schema: string;
        spatialNodes: number;
        elements: number;
        psets: number;
        qsets: number;
        aggregations: number;
        containments: number;
        materials: number;
        materialLayerSets: number;
        materialAssignments: number;
        recordsSeen: number;
      }>,
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
    /**
     * Attach a parsed IFC snapshot into the active project's
     * SQLCipher database. Counts are post-dedup: a re-attach of the
     * same file with identical content reports `_unchanged` instead
     * of `_inserted` / `_updated`.
     *
     * If the renderer just ran `importIfc(ifcPath)`, the bridge's
     * in-process snapshot cache will serve the parse for free
     * (`parseCacheHit: true`).
     */
    attachIfc: (projectPath: string, ifcPath: string) =>
      ipcRenderer.invoke("bim:attachIfc", { projectPath, ifcPath }) as Promise<{
        path: string;
        projectPath: string;
        parseCacheHit: boolean;
        spatialNodesInserted: number;
        spatialNodesUpdated: number;
        spatialNodesUnchanged: number;
        elementsInserted: number;
        elementsUpdated: number;
        elementsUnchanged: number;
        componentsInserted: number;
        relationsInserted: number;
        cacheRows: number;
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
      // Project label printed on the in-archive PDF summary. Must
      // be threaded through here (preload boundary) or the native
      // backend falls back to the generic "Project" label — see
      // `crates/aec_bridge/src/napi_api.rs::deliver_build_pack`.
      projectName?: string;
      includeRenders?: boolean;
      includeSheets?: boolean;
      includeIfc?: boolean;
      includeBoq?: boolean;
      includeProposal?: boolean;
      region?: "eu" | "na" | "apac";
    }) => ipcRenderer.invoke("deliver:buildPack", params),
  },

  // ----- Command engine -----
  // Typed bridge between the renderer-side command helpers
  // (`apps/desktop/renderer/src/api/commands.ts`) and the Rust
  // `aec_command::engine::CommandEngine`. Mirrors the inline shape
  // pattern used by `bim.attachIfc` / `bim.importIfc` above:
  // duplicated here rather than imported from `bridge.ts` because
  // Electron's context-isolation boundary forbids dragging the
  // main-process typing surface into the isolated world. Drift is
  // bounded by two compile-time guards (the IPC-handler return type
  // in `ipc.ts:command:apply` + the renderer-side `aec.ts` re-typing
  // against `CommandApplyResult`).
  command: {
    /**
     * Apply a typed command. `command` is the wire-shape envelope
     * (snake_case `command_id` / `ts` / `scope` / `actor` / `tool` /
     * `arguments`) produced by the renderer-side helpers.
     */
    apply: (projectPath: string, command: unknown) =>
      ipcRenderer.invoke("command:apply", { projectPath, command }) as Promise<{
        commandId: string;
        applied: Array<
          | { kind: "create"; record: { id: string; kind: string; parent: string | null; body: unknown } }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | { kind: "delete"; record: { id: string; kind: string; parent: string | null; body: unknown } }
        >;
        undoLen: number;
        redoLen: number;
      }>,
    /** Undo the most recently applied command. */
    undo: (projectPath: string, activeScope: string) =>
      ipcRenderer.invoke("command:undo", { projectPath, activeScope }) as Promise<{
        commandId: string;
        applied: Array<
          | { kind: "create"; record: { id: string; kind: string; parent: string | null; body: unknown } }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | { kind: "delete"; record: { id: string; kind: string; parent: string | null; body: unknown } }
        >;
        undoLen: number;
        redoLen: number;
      }>,
    /** Redo the most recently undone command. */
    redo: (projectPath: string, activeScope: string) =>
      ipcRenderer.invoke("command:redo", { projectPath, activeScope }) as Promise<{
        commandId: string;
        applied: Array<
          | { kind: "create"; record: { id: string; kind: string; parent: string | null; body: unknown } }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | { kind: "delete"; record: { id: string; kind: string; parent: string | null; body: unknown } }
        >;
        undoLen: number;
        redoLen: number;
      }>,
    /**
     * List the project graph. `kindFilter` narrows to a single
     * entity kind (e.g. `"wall"` / `"room"` / `"camera"`); pass
     * `undefined` for the full graph.
     */
    listGraph: (projectPath: string, kindFilter?: string) =>
      ipcRenderer.invoke("project:graphList", { projectPath, kindFilter }) as Promise<
        Array<{ id: string; kind: string; parent: string | null; body: unknown }>
      >,
  },

  // ----- Runtime -----
  runtime: {
    status: () => ipcRenderer.invoke("runtime:status"),
  },
};

contextBridge.exposeInMainWorld("aec", api);

export type AecApi = typeof api;
