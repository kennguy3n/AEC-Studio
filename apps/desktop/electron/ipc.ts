import { ipcMain } from "electron";
import { getBridge } from "./bridge";
import {
  clearActiveProjectPath,
  peekActiveProjectPath,
  resolveCurrentProjectForRenderer,
  setActiveProject,
  setActiveProjectIfMatchesActive,
} from "./active-project";

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
  ipcMain.handle(
    "project:createFromTemplate",
    async (_e, { templateKey, projectName }) => {
      assertString(templateKey, "templateKey");
      assertString(projectName, "projectName");
      // Promote the freshly created project to "active" so subsequent
      // draft / deliver / command handlers that don't carry an explicit
      // `projectPath` (because the renderer's public `aec.*` API doesn't
      // expose one) can resolve the active project path. See
      // `active-project.ts` for the design rationale.
      const summary = await getBridge().projectCreateFromTemplate(
        templateKey,
        projectName,
      );
      setActiveProject(summary);
      return summary;
    },
  );
  ipcMain.handle("project:open", async (_e, { projectPath }) => {
    assertString(projectPath, "projectPath");
    const summary = await getBridge().projectOpen(projectPath);
    // Same active-project promotion as `createFromTemplate`. Set
    // *after* `projectOpen` succeeds so a failed open (bad path,
    // wrong master key, etc.) doesn't leave a stale active project.
    setActiveProject(summary);
    return summary;
  });
  ipcMain.handle("project:save", async (_e, { projectPath }) => {
    assertString(projectPath, "projectPath");
    const summary = await getBridge().projectSave(projectPath);
    // Only refresh the cached active-project summary when the saved
    // project IS the currently-open one. The contract — "the active
    // slot only changes via project-lifecycle handlers, never as a
    // side-effect of a save of a non-active project" — is enforced
    // by `setActiveProjectIfMatchesActive`, shared with any future
    // handler that needs the same behaviour (e.g.
    // `project:exportPackage` if it grows a save side-effect). For
    // the active-project case the path is unchanged, so the
    // renderer-side hook treats this as the same project (no route
    // guard trip), it just re-renders the header timestamp from the
    // new `modifiedAt`.
    setActiveProjectIfMatchesActive(summary);
    return summary;
  });
  // `project:current` is a synchronous-style channel that the
  // renderer's `useActiveProject` hook polls (or subscribes via
  // an IPC push channel) to surface the currently-open project to
  // every page. Returns `null` when no project is open so the
  // hook can route the user to the Home screen via a route guard.
  //
  // The resolution logic — including the TOCTOU guard that prevents
  // a closed-during-await project from being silently re-activated
  // — lives in `resolveCurrentProjectForRenderer` on
  // `active-project.ts` so it can be unit-tested without Electron's
  // `ipcMain`. This handler is intentionally a thin one-liner.
  ipcMain.handle("project:current", async () => {
    return resolveCurrentProjectForRenderer(() =>
      getBridge().projectListRecents(),
    );
  });
  // `project:close` returns the user to the Home screen and clears
  // the active-project slot so subsequent `draft:*` / `deliver:*` /
  // `command:*` calls fail with the clean "no project is open"
  // validation error rather than addressing the previously-open
  // project. The renderer's `useActiveProject.closeProject()`
  // calls this and then navigates to `/`. No bridge call is
  // required — the project package is already persisted on disk
  // (Save flushes on every mutation, and `projectSave` is
  // idempotent).
  ipcMain.handle("project:close", async () => {
    clearActiveProjectPath();
    return { ok: true };
  });
  ipcMain.handle("project:listRecents", async () => {
    return getBridge().projectListRecents();
  });
  ipcMain.handle(
    "project:exportPackage",
    async (_e, { projectPath, outPath }) => {
      assertString(projectPath, "projectPath");
      assertString(outPath, "outPath");
      return getBridge().projectExportPackage(projectPath, outPath);
    },
  );

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
  // Every `draft:*` handler resolves a `projectPath` before calling
  // the bridge. The renderer-side `aec.draft.*` shape doesn't expose
  // `projectPath` (pages don't track which project is open — the
  // main process does), so we inject it from the active-project
  // tracker. If a caller *does* include `projectPath` in the params
  // (e.g. a renderer test that prefers to be explicit), the
  // caller-supplied value wins.
  ipcMain.handle("draft:drawPrimitive", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftDrawPrimitive(
      withResolvedProjectPath(p, "draftDrawPrimitive"),
    );
  });
  ipcMain.handle("draft:editTool", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftEditTool(
      withResolvedProjectPath(p, "draftEditTool"),
    );
  });
  ipcMain.handle("draft:createSheet", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftCreateSheet(
      withResolvedProjectPath(p, "draftCreateSheet"),
    );
  });
  ipcMain.handle("draft:setLayerState", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().draftSetLayerState(
      withResolvedProjectPath(p, "draftSetLayerState"),
    );
  });
  // DXF import / export use the same `withResolvedProjectPath` shape
  // as the other `draft:*` handlers above. The renderer-facing preload
  // signature is `importDxf({ dxfPath, projectPath? })`. If the caller
  // supplies an explicit `projectPath` (e.g. a renderer test) it
  // wins; otherwise the active-project tracker provides it. We
  // additionally validate that `dxfPath` is a non-empty string so a
  // missing field surfaces a typed error at the IPC boundary rather
  // than as a NAPI string-conversion failure on the Rust side.
  ipcMain.handle("draft:importDxf", async (_e, p) => {
    assertObject(p, "params");
    assertString((p as { dxfPath?: unknown }).dxfPath, "dxfPath");
    return getBridge().draftImportDxf(
      withResolvedProjectPath(p, "draftImportDxf") as {
        projectPath: string;
        dxfPath: string;
      },
    );
  });
  ipcMain.handle("draft:exportDxf", async (_e, p) => {
    assertObject(p, "params");
    assertString((p as { dxfPath?: unknown }).dxfPath, "dxfPath");
    return getBridge().draftExportDxf(
      withResolvedProjectPath(p, "draftExportDxf") as {
        projectPath: string;
        dxfPath: string;
      },
    );
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
  ipcMain.handle("bim:attachIfc", async (_e, { projectPath, ifcPath }) => {
    assertString(projectPath, "projectPath");
    assertString(ifcPath, "ifcPath");
    return getBridge().bimAttachIfc(projectPath, ifcPath);
  });
  ipcMain.handle("bim:exportIfc", async (_e, p) => {
    assertObject(p, "params");
    const sourcePath = (p as { sourcePath?: unknown }).sourcePath;
    const outPath = (p as { outPath?: unknown }).outPath;
    assertString(sourcePath, "sourcePath");
    assertString(outPath, "outPath");
    return getBridge().bimExportIfc({ sourcePath, outPath });
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
    const sourcePath = (p as { sourcePath?: unknown }).sourcePath;
    const outPath = (p as { outPath?: unknown }).outPath;
    const kind = (p as { kind?: unknown }).kind;
    assertString(sourcePath, "sourcePath");
    assertString(outPath, "outPath");
    assertString(kind, "kind");
    if (
      kind !== "door" &&
      kind !== "window" &&
      kind !== "room" &&
      kind !== "material"
    ) {
      throw new Error(
        `bimGenerateSchedule: kind must be one of door/window/room/material, got ${kind}`,
      );
    }
    return getBridge().bimGenerateSchedule({ sourcePath, outPath, kind });
  });
  // `bim:readScheduleRows` parses an XLSX schedule file the bridge
  // wrote during `bim_generate_schedule` and returns the rows so
  // the renderer's `ScheduleView` can display them without having
  // to re-read the file from the renderer process. The bridge
  // crate already exposes a `read_schedule_rows` helper; this
  // handler is a thin wrapper that validates the path.
  ipcMain.handle("bim:readScheduleRows", async (_e, p) => {
    assertObject(p, "params");
    const xlsxPath = (p as { xlsxPath?: unknown }).xlsxPath;
    assertString(xlsxPath, "xlsxPath");
    return getBridge().bimReadScheduleRows({ xlsxPath });
  });
  ipcMain.handle("bim:validate", async (_e, p) => {
    assertObject(p, "params");
    const sourcePath = (p as { sourcePath?: unknown }).sourcePath;
    assertString(sourcePath, "sourcePath");
    return getBridge().bimValidate({ sourcePath });
  });
  ipcMain.handle("bim:diff", async (_e, p) => {
    assertObject(p, "params");
    const beforePath = (p as { beforePath?: unknown }).beforePath;
    const afterPath = (p as { afterPath?: unknown }).afterPath;
    assertString(beforePath, "beforePath");
    assertString(afterPath, "afterPath");
    return getBridge().bimDiff({ beforePath, afterPath });
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
      throw new Error(
        "renderEnqueueBatch: cameraIds must be a non-empty array",
      );
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
  // `ai:plan` is routed through `withResolvedProjectPath` for the same
  // reason `draft:*` / `deliver:*` are: the renderer's public preload
  // shape (`aec.ai.plan({ tool, scope, prompt, context,
  // maxEntitiesModified })`) does NOT carry `projectPath`, but the
  // native adaptor (`adaptNative.aiPlan` in `bridge.ts`) treats it
  // as a required string and throws when it's missing. Routing
  // through the helper injects the active project path from the
  // tracker at *plan-time* (the moment the user pressed "Suggest
  // layout"), which is also the binding semantics the Rust side
  // wants: `BridgeService::ai_plan` captures `(project_path, scope)`
  // into the `PendingDiff` registry, and `ai_accept_diff` later
  // re-opens that exact project even if the renderer has since
  // switched the active project. Without this injection, the
  // renderer call at `LayoutSuggestionsPanel.tsx::suggestLayout`
  // would surface "aiPlan: missing required string field
  // 'projectPath'" in production while still passing the in-process
  // backend test at `bridge-ai.test.ts:81-97` (the in-process
  // fallback intentionally accepts missing `projectPath`).
  ipcMain.handle("ai:plan", async (_e, p) => {
    assertObject(p, "params");
    return getBridge().aiPlan(withResolvedProjectPath(p, "aiPlan"));
  });
  ipcMain.handle("ai:acceptDiff", async (_e, { diffId }) => {
    assertString(diffId, "diffId");
    return getBridge().aiAcceptDiff(diffId);
  });
  ipcMain.handle("ai:rejectDiff", async (_e, payload) => {
    assertObject(payload, "params");
    const { diffId, reason } = payload as {
      diffId?: unknown;
      reason?: unknown;
    };
    assertString(diffId, "diffId");
    // `reason` is optional; the renderer can omit it for silent
    // rejections (e.g. user dismissed the panel). When present it
    // must be a string \u2014 reject anything else loudly so the
    // forensic log never sees `[object Object]` or NaN.
    let reasonStr: string | null = null;
    if (reason !== undefined && reason !== null) {
      if (typeof reason !== "string") {
        throw new Error(
          "ai:rejectDiff: 'reason' must be a string when present",
        );
      }
      reasonStr = reason;
    }
    return getBridge().aiRejectDiff(diffId, reasonStr);
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
    // Inject the active project path so the bridge can thread real
    // room / material / template counts and the floor-plan SVG into
    // the proposal cover. Caller-supplied wins; otherwise the
    // active-project tracker is the source of truth. Mirrors the
    // `deliver:buildPack` handler's resolution semantics.
    const projectPath =
      typeof p.projectPath === "string" && p.projectPath.length > 0
        ? p.projectPath
        : peekActiveProjectPath();
    const merged: Record<string, unknown> = { ...p };
    if (projectPath) {
      merged.projectPath = projectPath;
    }
    return getBridge().exportBuildProposalPack(merged);
  });

  // ----- Deliver -----
  // The three `deliver:*` handlers resolve `projectPath` through the
  // same `withResolvedProjectPath` helper the `draft:*` handlers use
  // above. The renderer's public preload doesn't expose `projectPath`
  // on these channels today (so in practice the tracker always wins),
  // but routing through the helper means a future renderer call (or
  // a test) that *does* pass an explicit `projectPath` will have it
  // honoured — same caller-supplied-wins semantics as `draft:*`
  // without a special case. Resolves PR-X round 4 ANALYSIS-0002
  // (deliver / draft handler asymmetry).
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
    const resolved = withResolvedProjectPath(
      { ...p, description, entities },
      "deliverCreateRevision",
    );
    return getBridge().deliverCreateRevision({
      projectPath: resolved.projectPath as string,
      tag: p.tag as string,
      description,
      entities,
    });
  });
  ipcMain.handle("deliver:listRevisions", async (_e, p) => {
    // `aec.deliver.listRevisions()` is called with no arguments from
    // the renderer, so `p` is typically `undefined`. Normalize to an
    // empty object before resolving the project path so the helper
    // can apply the standard caller-supplied-wins semantics.
    const params: Record<string, unknown> =
      p !== null && typeof p === "object" && !Array.isArray(p)
        ? (p as Record<string, unknown>)
        : {};
    const resolved = withResolvedProjectPath(params, "deliverListRevisions");
    return getBridge().deliverListRevisions({
      projectPath: resolved.projectPath as string,
    });
  });
  ipcMain.handle("deliver:compareRevisions", async (_e, p) => {
    assertObject(p, "params");
    assertString(p.baseId, "baseId");
    assertString(p.headId, "headId");
    const resolved = withResolvedProjectPath(p, "deliverCompareRevisions");
    return getBridge().deliverCompareRevisions({
      projectPath: resolved.projectPath as string,
      baseId: p.baseId as string,
      headId: p.headId as string,
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
    // Inject the active project path so the bridge can build a
    // `DeliverPackContext` from the real project (renders dir,
    // schedules from graph, sheets, IFC string). Caller-supplied
    // `projectPath` wins (e.g. an explicit "export this archived
    // project" flow); otherwise the active-project tracker is the
    // source of truth.
    const projectPath =
      typeof p.projectPath === "string" && p.projectPath.length > 0
        ? p.projectPath
        : peekActiveProjectPath();
    return getBridge().deliverBuildPack({
      kind: p.kind as "concept" | "interior" | "contractor" | "bim",
      outPath: p.outPath,
      // Forwarded so the native backend's PDF-summary label matches
      // the renderer-supplied project name (otherwise napi defaults
      // to the generic "Project" string).
      projectName:
        typeof p.projectName === "string" ? p.projectName : undefined,
      projectPath: projectPath ?? undefined,
      includeRenders:
        typeof p.includeRenders === "boolean" ? p.includeRenders : undefined,
      includeSheets:
        typeof p.includeSheets === "boolean" ? p.includeSheets : undefined,
      includeIfc: typeof p.includeIfc === "boolean" ? p.includeIfc : undefined,
      includeBoq: typeof p.includeBoq === "boolean" ? p.includeBoq : undefined,
      includeProposal:
        typeof p.includeProposal === "boolean" ? p.includeProposal : undefined,
      region,
    });
  });

  // ----- Command engine -----
  // `command:apply` / `command:undo` / `command:redo` route the
  // renderer's typed command envelopes to the Rust command engine
  // through `aec_bridge::napi_api::command_*`. The IPC layer is
  // intentionally thin — every field on the `command` payload is
  // validated structurally on the Rust side (serde + dispatch); we
  // only assert shape pre-conditions here so a malformed renderer
  // call fails fast at the boundary.
  ipcMain.handle("command:apply", async (_e, { projectPath, command }) => {
    assertString(projectPath, "projectPath");
    assertObject(command, "command");
    const c = command as Record<string, unknown>;
    assertString(c.command_id as unknown, "command.command_id");
    assertString(c.tool as unknown, "command.tool");
    // The envelope's `scope` must be one of the five workflow scopes so
    // an invalid value (e.g. a renderer typo or stale call site) fails
    // fast at the IPC boundary rather than as an opaque serde error on
    // the Rust side or as a silent accept by the in-process fallback.
    assertScope(c.scope, "command.scope");
    return getBridge().commandApply(
      projectPath,
      command as unknown as import("./bridge").Command,
    );
  });
  ipcMain.handle("command:undo", async (_e, { projectPath, activeScope }) => {
    assertString(projectPath, "projectPath");
    assertScope(activeScope);
    return getBridge().commandUndo(projectPath, activeScope);
  });
  ipcMain.handle("command:redo", async (_e, { projectPath, activeScope }) => {
    assertString(projectPath, "projectPath");
    assertScope(activeScope);
    return getBridge().commandRedo(projectPath, activeScope);
  });
  ipcMain.handle(
    "project:graphList",
    async (_e, { projectPath, kindFilter }) => {
      assertString(projectPath, "projectPath");
      if (kindFilter !== undefined && kindFilter !== null) {
        assertString(kindFilter, "kindFilter");
      }
      return getBridge().projectGraphList(
        projectPath,
        typeof kindFilter === "string" ? kindFilter : undefined,
      );
    },
  );

  // ----- Runtime -----
  ipcMain.handle("runtime:status", async () => getBridge().runtimeStatus());

  // ----- KChat (Phase 12) -----
  ipcMain.handle("kchat:status", async () => getBridge().kchatStatus());
  ipcMain.handle("kchat:reload", async () => getBridge().kchatReload());
  ipcMain.handle("kchat:publish", async (_e, params) => {
    assertObject(params, "params");
    const cardJson = (params as { cardJson?: unknown }).cardJson;
    assertString(cardJson, "cardJson");
    // Surface obviously-broken payloads at the IPC boundary so the
    // caller gets a descriptive error rather than a generic
    // GenericFailure from `serde_json::from_str` on the Rust side.
    try {
      const parsed = JSON.parse(cardJson) as unknown;
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
        throw new Error(
          "kchatPublish: cardJson must encode an object (ArtifactCard)",
        );
      }
    } catch (err) {
      if (err instanceof SyntaxError) {
        throw new Error(
          `kchatPublish: cardJson is not valid JSON (${err.message})`,
        );
      }
      throw err;
    }
    return getBridge().kchatPublish({ cardJson });
  });
  ipcMain.handle("kchat:ingestReviews", async (_e, params) => {
    assertObject(params, "params");
    const threadId = (params as { threadId?: unknown }).threadId;
    assertString(threadId, "threadId");
    const rawSince = (params as { sinceIso?: unknown }).sinceIso;
    let sinceIso: string | null | undefined;
    if (rawSince === undefined || rawSince === null) {
      sinceIso = rawSince ?? undefined;
    } else if (typeof rawSince === "string") {
      sinceIso = rawSince;
    } else {
      throw new Error("kchatIngestReviews: sinceIso must be a string or null");
    }
    return getBridge().kchatIngestReviews({ threadId, sinceIso });
  });

  // ----- Viewport (Phase 12) -----
  //
  // The viewport handlers are thin: they validate the IPC payload
  // shape and forward to the bridge. The bridge's
  // `viewport_service` owns the wgpu device + render pipeline; it
  // degrades gracefully to a "no GPU" stub when no adapter is
  // available (CI / headless), so the renderer can call these
  // handlers unconditionally without checking for hardware support
  // first.
  ipcMain.handle("viewport:status", async () => getBridge().viewportStatus());
  ipcMain.handle("viewport:resize", async (_e, params) => {
    assertObject(params, "params");
    const p = params as { width?: unknown; height?: unknown };
    const width = Number(p.width);
    const height = Number(p.height);
    if (!Number.isFinite(width) || width <= 0) {
      throw new Error("viewport:resize requires a positive numeric width");
    }
    if (!Number.isFinite(height) || height <= 0) {
      throw new Error("viewport:resize requires a positive numeric height");
    }
    return getBridge().viewportResize({
      width: Math.round(width),
      height: Math.round(height),
    });
  });
  ipcMain.handle("viewport:input", async (_e, params) => {
    assertObject(params, "params");
    const p = params as {
      kind?: unknown;
      dx?: unknown;
      dy?: unknown;
      delta?: unknown;
    };
    if (
      p.kind !== "orbit" &&
      p.kind !== "pan" &&
      p.kind !== "zoom" &&
      p.kind !== "reset"
    ) {
      throw new Error(
        "viewport:input `kind` must be 'orbit' | 'pan' | 'zoom' | 'reset'",
      );
    }
    return getBridge().viewportInput({
      kind: p.kind,
      dx: typeof p.dx === "number" ? p.dx : undefined,
      dy: typeof p.dy === "number" ? p.dy : undefined,
      delta: typeof p.delta === "number" ? p.delta : undefined,
    });
  });
  ipcMain.handle("viewport:requestFrame", async () =>
    getBridge().viewportRequestFrame(),
  );
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

/**
 * Return a copy of `params` with `projectPath` filled in. If the
 * caller already supplied `projectPath` in the params (e.g. a
 * renderer test or a future page that wants to override the active
 * project), that value is preserved; otherwise we inject the
 * tracked active project path. Throws `IpcValidationError` if
 * neither source has a path (i.e. no project is currently open).
 *
 * Centralising this here keeps every `draft:*` handler that takes
 * a `Record<string, unknown>` from re-implementing the same merge
 * logic (and possibly skipping it).
 */
function withResolvedProjectPath(
  params: Record<string, unknown>,
  method: string,
): Record<string, unknown> {
  if (typeof params.projectPath === "string" && params.projectPath.length > 0) {
    return params;
  }
  const active = peekActiveProjectPath();
  if (active === null) {
    throw new IpcValidationError(
      `${method}: no project is currently open. Call ` +
        `\`project.open\` or \`project.createFromTemplate\` first.`,
    );
  }
  return { ...params, projectPath: active };
}

/**
 * Validate that `value` is one of the five workflow scopes the
 * `aec_command` engine recognises. We narrow to a string literal
 * union here so the call into `commandUndo` / `commandRedo` is
 * typesafe and a typo from the renderer surfaces at the IPC
 * boundary rather than as a deserialisation error on the Rust
 * side.
 */
export function assertScope(
  value: unknown,
  field: string = "activeScope",
): asserts value is import("./bridge").CommandScope {
  const allowed = ["design", "draft", "bim", "render", "deliver"] as const;
  if (
    typeof value !== "string" ||
    !allowed.includes(value as (typeof allowed)[number])
  ) {
    throw new IpcValidationError(
      `${field} must be one of ${allowed.join(" / ")} (got: ${String(value)})`,
    );
  }
}

export class IpcValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "IpcValidationError";
  }
}
