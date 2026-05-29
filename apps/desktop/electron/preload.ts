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
      ipcRenderer.invoke("project:createFromTemplate", {
        templateKey,
        projectName,
      }),
    open: (projectPath: string) =>
      ipcRenderer.invoke("project:open", { projectPath }),
    save: (projectPath: string) =>
      ipcRenderer.invoke("project:save", { projectPath }),
    listRecents: () => ipcRenderer.invoke("project:listRecents"),
    exportPackage: (projectPath: string, outPath: string) =>
      ipcRenderer.invoke("project:exportPackage", { projectPath, outPath }),
    current: () =>
      ipcRenderer.invoke("project:current") as Promise<{
        summary: {
          projectId: string;
          name: string;
          path: string;
          templateKey: string | null;
          modifiedAt: string;
        } | null;
      }>,
    close: () =>
      ipcRenderer.invoke("project:close") as Promise<{ ok: true }>,
    // Push subscription to active-project changes. The main process
    // (`active-project.ts → onActiveProjectChange`) calls every
    // listener synchronously on every `setActive*` / `clear*` site;
    // the IPC bridge fans those notifications out to every renderer
    // window via `webContents.send("project:active-changed", summary)`.
    //
    // The renderer's `useActiveProject` hook subscribes once on mount
    // so that future multi-window scenarios — a second BrowserWindow
    // is added (e.g. "Open project in new window", Print Preview,
    // pop-out viewport), a future test harness mutates the tracker
    // directly, or the bridge auto-recovers by routing to a fallback
    // project after a corrupt-DB read — all keep the renderer-side
    // active-project state coherent without a full reload.
    //
    // Today there is a single renderer window so the listener is a
    // defensive no-op for any push that originated from the same
    // window's own `openProject` / `createProject` / `saveProject` /
    // `closeProject` (the renderer already updated its state before
    // the push round-tripped back). The push channel does NOT
    // duplicate the renderer-only fields (`dirty`, `saving`,
    // `undoLen`, `redoLen`) — those are window-local and stay in the
    // renderer's `useActiveProject` state.
    //
    // Returns an unsubscribe function. The hook calls it in its
    // `useEffect` cleanup to prevent leaked listeners across HMR
    // reloads in dev. We use `ipcRenderer.on` (NOT `addListener` /
    // `once`) because the channel emits indefinitely.
    onActiveProjectChange: (
      listener: (
        summary: {
          projectId: string;
          name: string;
          path: string;
          templateKey: string | null;
          modifiedAt: string;
        } | null,
      ) => void,
    ): (() => void) => {
      const channel = "project:active-changed";
      const handler = (
        _event: Electron.IpcRendererEvent,
        summary: {
          projectId: string;
          name: string;
          path: string;
          templateKey: string | null;
          modifiedAt: string;
        } | null,
      ) => {
        listener(summary);
      };
      ipcRenderer.on(channel, handler);
      return () => {
        ipcRenderer.removeListener(channel, handler);
      };
    },
  },

  // ----- Dialog (Phase 13) -----
  dialog: {
    openFile: (params?: {
      title?: string;
      defaultPath?: string;
      filters?: Array<{ name: string; extensions: string[] }>;
      message?: string;
      allowMultiple?: boolean;
    }) =>
      ipcRenderer.invoke("dialog:openFile", params ?? {}) as Promise<{
        canceled: boolean;
        paths: string[];
      }>,
    openDirectory: (params?: {
      title?: string;
      defaultPath?: string;
      message?: string;
    }) =>
      ipcRenderer.invoke("dialog:openDirectory", params ?? {}) as Promise<{
        canceled: boolean;
        path: string | null;
      }>,
    saveFile: (params?: {
      title?: string;
      defaultPath?: string;
      filters?: Array<{ name: string; extensions: string[] }>;
      message?: string;
    }) =>
      ipcRenderer.invoke("dialog:saveFile", params ?? {}) as Promise<{
        canceled: boolean;
        path: string | null;
      }>,
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
    // Symmetric with the other `draft:*` handlers — accepts the full
    // bridge param object `{ dxfPath, projectPath? }`. If `projectPath`
    // is omitted, the IPC handler resolves it from the active-project
    // tracker (same `withResolvedProjectPath` pattern as
    // drawPrimitive / editTool / createSheet / setLayerState).
    importDxf: (params: { dxfPath: string; projectPath?: string }) =>
      ipcRenderer.invoke("draft:importDxf", params),
    exportDxf: (params: { dxfPath: string; projectPath?: string }) =>
      ipcRenderer.invoke("draft:exportDxf", params),
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
    /**
     * Re-serialise an IFC file via the native bridge. Same writer
     * as `attachIfc` so the output is byte-identical to what a
     * `bimAttachIfc` snapshot would write. The inline type mirrors
     * `BimExportIfcSummary` in `electron/bridge.ts` 1:1 — drift
     * here would surface as `undefined` on the export panel, so
     * the bridge's `adaptNative` self-check guards against it.
     */
    exportIfc: (params: { sourcePath: string; outPath: string }) =>
      ipcRenderer.invoke("bim:exportIfc", params) as Promise<{
        sourcePath: string;
        outPath: string;
        schema: string;
        bytesWritten: number;
        parseCacheHit: boolean;
      }>,
    /**
     * Run `bim_classify` on the active project's entity graph for
     * the requested `scheme` (`"ifc"` / `"uniformat-ii"` /
     * `"omniclass-21"`). The inline result type mirrors
     * `BimClassifyResult` in `electron/bridge.ts` 1:1 so the
     * renderer can react to counts without an extra import; drift
     * here would surface as `undefined` on the classify toast,
     * which the bridge's `adaptNative` self-check guards against.
     */
    classify: (params: { projectPath: string; scheme: string }) =>
      ipcRenderer.invoke("bim:classify", params) as Promise<{
        scheme: string;
        classified: number;
        unchanged: number;
        skipped: number;
        details: Array<{
          entityId: string;
          code: string;
          title: string;
        }>;
      }>,
    setProperty: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("bim:setProperty", params),
    /**
     * Generate an XLSX schedule of a given kind (door / window /
     * room / material). Inline type mirrors `BimScheduleSummary`
     * in `electron/bridge.ts` 1:1.
     */
    readScheduleRows: (params: { xlsxPath: string }) =>
      ipcRenderer.invoke("bim:readScheduleRows", params) as Promise<{
        // `header` carries the writer's column order. The bridge
        // returns rows as `Record<string, string>` (an unordered
        // map at the napi boundary), so without `header` the
        // renderer would have to derive column order from
        // `Object.keys(rows[0])`, which is non-deterministic across
        // V8 builds for non-integer string keys. Threading
        // `header` through preserves the deterministic ordering
        // the schedule writer emitted.
        header: string[];
        rows: Array<Record<string, string>>;
      }>,
    generateSchedule: (params: {
      sourcePath: string;
      outPath: string;
      kind: "door" | "window" | "room" | "material";
    }) =>
      ipcRenderer.invoke("bim:generateSchedule", params) as Promise<{
        scheduleId: string;
        kind: "door" | "window" | "room" | "material";
        sourcePath: string;
        outPath: string;
        rows: number;
        columns: number;
        bytesWritten: number;
        parseCacheHit: boolean;
      }>,
    /**
     * Run the rule-based BIM validator over an IFC file. Inline
     * type mirrors `BimValidateReport` in `electron/bridge.ts`
     * 1:1.
     */
    validate: (params: { sourcePath: string }) =>
      ipcRenderer.invoke("bim:validate", params) as Promise<{
        ok: boolean;
        sourcePath: string;
        schema: string;
        errors: Array<{
          severity: "error" | "warning" | "info";
          code: string;
          element: string | null;
          description: string;
          suggestion: string | null;
        }>;
        warnings: Array<{
          severity: "error" | "warning" | "info";
          code: string;
          element: string | null;
          description: string;
          suggestion: string | null;
        }>;
        infos: Array<{
          severity: "error" | "warning" | "info";
          code: string;
          element: string | null;
          description: string;
          suggestion: string | null;
        }>;
        parseCacheHit: boolean;
      }>,
    /**
     * Diff two IFC files. Inline type mirrors `BimDiffSummary`
     * in `electron/bridge.ts` 1:1.
     */
    diff: (params: { beforePath: string; afterPath: string }) =>
      ipcRenderer.invoke("bim:diff", params) as Promise<{
        diffId: string;
        beforePath: string;
        afterPath: string;
        beforeSchema: string;
        afterSchema: string;
        added: string[];
        removed: string[];
        modified: Array<{
          key: string;
          classBefore: string | null;
          classAfter: string | null;
          nameBefore: string | null;
          nameAfter: string | null;
          propertyDeltas: Array<{
            pset: string;
            key: string;
            before: string | null;
            after: string | null;
          }>;
        }>;
        beforeCacheHit: boolean;
        afterCacheHit: boolean;
      }>,
  },

  // ----- Render -----
  render: {
    enqueueRender: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("render:enqueueRender", params),
    listJobs: () => ipcRenderer.invoke("render:listJobs"),
    cancelJob: (jobId: string) =>
      ipcRenderer.invoke("render:cancelJob", { jobId }),
    applyPreset: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("render:applyPreset", params),
    diagnose: (jobId: string) =>
      ipcRenderer.invoke("render:diagnose", { jobId }),
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
    plan: (params: Record<string, unknown>) =>
      ipcRenderer.invoke("ai:plan", params),
    acceptDiff: (diffId: string) =>
      ipcRenderer.invoke("ai:acceptDiff", { diffId }),
    rejectDiff: (diffId: string, reason?: string | null) =>
      ipcRenderer.invoke("ai:rejectDiff", { diffId, reason: reason ?? null }),
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
      // Explicit project path for the "archived project" flow
      // documented in `bridge.ts`. When omitted the main-process IPC
      // handler falls back to `peekActiveProjectPath()` (the active
      // project). Aligning the preload type with the handler keeps
      // the renderer-side call type-safe end-to-end instead of
      // relying on structural-typing leakage at the IPC boundary.
      projectPath?: string;
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
          | {
              kind: "create";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | {
              kind: "delete";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
        >;
        undoLen: number;
        redoLen: number;
      }>,
    /** Undo the most recently applied command. */
    undo: (projectPath: string, activeScope: string) =>
      ipcRenderer.invoke("command:undo", {
        projectPath,
        activeScope,
      }) as Promise<{
        commandId: string;
        applied: Array<
          | {
              kind: "create";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | {
              kind: "delete";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
        >;
        undoLen: number;
        redoLen: number;
      }>,
    /** Redo the most recently undone command. */
    redo: (projectPath: string, activeScope: string) =>
      ipcRenderer.invoke("command:redo", {
        projectPath,
        activeScope,
      }) as Promise<{
        commandId: string;
        applied: Array<
          | {
              kind: "create";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
          | { kind: "update"; id: string; before: unknown; after: unknown }
          | {
              kind: "delete";
              record: {
                id: string;
                kind: string;
                parent: string | null;
                body: unknown;
              };
            }
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
      ipcRenderer.invoke("project:graphList", {
        projectPath,
        kindFilter,
      }) as Promise<
        Array<{
          id: string;
          kind: string;
          parent: string | null;
          body: unknown;
        }>
      >,
  },

  // ----- Runtime -----
  runtime: {
    status: () => ipcRenderer.invoke("runtime:status"),
  },

  // ----- KChat (Phase 15: loopback HTTP + .kcz extension) -----
  //
  // Phase 12's socket / named-pipe transport is gone; the
  // `publisherKind` discriminator is now `loopback_http`
  // (production) or `in_memory` (tests / headless). The
  // `instanceJson` shape changed too — see
  // `bridge.ts:KChatStatusReport` for the new schema.
  kchat: {
    status: () =>
      ipcRenderer.invoke("kchat:status") as Promise<{
        state: "connected" | "reconnecting" | "disconnected";
        publisherKind: "loopback_http" | "in_memory";
        instanceJson: string | null;
        defaultThreadId: string | null;
        enabled: boolean;
      }>,
    reload: () =>
      ipcRenderer.invoke("kchat:reload") as Promise<{
        state: "connected" | "reconnecting" | "disconnected";
        publisherKind: "loopback_http" | "in_memory";
        instanceJson: string | null;
        defaultThreadId: string | null;
        enabled: boolean;
      }>,
    /**
     * Flip the bridge-persisted KChat enable switch. Returns the
     * fresh status snapshot so the Settings card can update its
     * UI without a follow-up `status()` poll.
     */
    setEnabled: (params: { enabled: boolean }) =>
      ipcRenderer.invoke("kchat:setEnabled", params) as Promise<{
        state: "connected" | "reconnecting" | "disconnected";
        publisherKind: "loopback_http" | "in_memory";
        instanceJson: string | null;
        defaultThreadId: string | null;
        enabled: boolean;
      }>,
    publish: (params: { cardJson: string }) =>
      ipcRenderer.invoke("kchat:publish", params) as Promise<{
        messageId: string;
        threadId: string;
        publishedAt: string;
      }>,
    ingestReviews: (params: { threadId: string; sinceIso?: string | null }) =>
      ipcRenderer.invoke("kchat:ingestReviews", params) as Promise<{
        threadId: string;
        commentsJson: string;
        cardsJson: string;
      }>,
    /**
     * Open a `kchat://app/...` deeplink in the OS-registered
     * KChat Desktop binary. Rate-limited; scheme-checked.
     * Returns `{ ok: false, reason }` when blocked.
     */
    openInDesktop: (params: { url: string }) =>
      ipcRenderer.invoke("kchat:openInDesktop", params) as Promise<{
        ok: boolean;
        reason?: "rate_limited" | "scheme_not_allowed";
      }>,
    /**
     * Subscribe to inbound `aecstudio://...` deeplinks parsed
     * by the main-process bridge. The callback receives the
     * typed `DeeplinkRoute` directly; unsubscribe via the
     * returned disposer.
     */
    onDeeplink: (
      cb: (
        route:
          | { kind: "review"; threadId: string }
          | { kind: "project"; projectId: string }
          | { kind: "deliver"; packId: string },
      ) => void,
    ) => {
      const handler = (
        _e: Electron.IpcRendererEvent,
        route:
          | { kind: "review"; threadId: string }
          | { kind: "project"; projectId: string }
          | { kind: "deliver"; packId: string },
      ): void => cb(route);
      ipcRenderer.on("kchat:deeplink", handler);
      return () => {
        ipcRenderer.removeListener("kchat:deeplink", handler);
      };
    },
  },

  // ----- Viewport (Phase 12) -----
  //
  // The viewport API is intentionally orthogonal to the other
  // bridge methods: it doesn't go through the `bridge.*` namespace
  // (which is in-process-fallback-or-native) because the viewport
  // service has its own state (no `BridgeService` cache needed)
  // and we don't want the in-process fallback to be reachable
  // through `aec.viewport.*` — when there's no native bridge,
  // the Design viewport should explicitly render a "GPU
  // unavailable" placeholder rather than animate against a stub.
  viewport: {
    status: () =>
      ipcRenderer.invoke("viewport:status") as Promise<{
        state: "ready" | "unavailable";
        width: number;
        height: number;
        frameIndex: number;
        gpuDescriptorJson?: string | null;
      }>,
    resize: (params: { width: number; height: number }) =>
      ipcRenderer.invoke("viewport:resize", params) as Promise<{
        state: "ready" | "unavailable";
        width: number;
        height: number;
        frameIndex: number;
        gpuDescriptorJson?: string | null;
      }>,
    input: (params: {
      kind: "orbit" | "pan" | "zoom" | "reset";
      dx?: number;
      dy?: number;
      delta?: number;
    }) =>
      ipcRenderer.invoke("viewport:input", params) as Promise<{
        cameraJson: string;
      }>,
    requestFrame: () =>
      ipcRenderer.invoke("viewport:requestFrame") as Promise<{
        frameIndex: number;
        width: number;
        height: number;
        state: "presented" | "coalesced" | "unavailable";
        cameraJson: string;
      }>,
  },
};

contextBridge.exposeInMainWorld("aec", api);

export type AecApi = typeof api;
