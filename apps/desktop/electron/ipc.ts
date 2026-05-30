import { ipcMain } from "electron";
import { randomUUID } from "node:crypto";
import { getBridge } from "./bridge";
import {
  clearActiveProjectPath,
  peekActiveProjectPath,
  resolveCurrentProjectForRenderer,
  setActiveProject,
  setActiveProjectIfMatchesActive,
} from "./active-project";
import {
  enqueuePublish,
  getKchatRendererSnapshot,
  getReviewCommentsForThread,
} from "./kchat/kchatAppState";
import { openKchatDeeplink } from "./kchat/kchatOutboundDeeplink";
import { mapStoredReviewCommentsToRows } from "./kchat/reviewWireFormat";

/**
 * Heartbeat-freshness window for the loopback API's
 * `connected`/`reconnecting`/`disconnected` ternary. 30 s covers an
 * entire `.kcz` extension activation cycle plus the bot's
 * idle-polling cadence; tighter than this would flap, looser would
 * mask a dead extension for too long.
 */
const KCHAT_HEARTBEAT_FRESH_MS = 30_000;

/**
 * Build the renderer-facing kchat status payload.
 *
 * Centralised so the `kchat:status` IPC handler and the
 * `kchat:reload` handler compute the connection state with
 * identical logic. Previously `kchat:reload` short-circuited to
 * `apiServerRunning ? "reconnecting" : "disconnected"`, which
 * meant clicking "Re-detect" on the Settings card with a healthy
 * extension flashed the indicator to `reconnecting` until the next
 * 5 s status poll healed it — visible UX bug.
 *
 * The Phase 15 transport is local-only (no socket reconnect to
 * negotiate), so "reload" is just "snapshot the current state".
 */
function buildKchatStatusResponse(
  defaultThreadId: string | null,
  enabled: boolean,
): {
  state: "connected" | "reconnecting" | "disconnected";
  publisherKind: "loopback_http";
  instanceJson: string;
  defaultThreadId: string | null;
  enabled: boolean;
} {
  const snap = getKchatRendererSnapshot();
  const nowMs = Date.now();
  const lastMs =
    snap.lastExtensionContactAt === null
      ? null
      : Date.parse(snap.lastExtensionContactAt);
  let state: "connected" | "reconnecting" | "disconnected";
  if (!enabled) {
    // The bridge-persisted master toggle is off. The Settings
    // toggle / per-project manifest flipped it; the
    // `kchat:publish` handler refuses publishes in this state
    // before they ever reach the loopback queue. Forcing
    // `disconnected` here keeps the renderer's chip honest — a
    // running loopback server with a fresh extension heartbeat
    // would otherwise read "online" while every publish bounces
    // off the gate.
    state = "disconnected";
  } else if (!snap.apiServerRunning) {
    // The server itself isn't up — typically a boot failure or a
    // shutdown in progress. The renderer treats this as a hard
    // disconnect and hides the Settings affordances.
    state = "disconnected";
  } else if (lastMs !== null && nowMs - lastMs < KCHAT_HEARTBEAT_FRESH_MS) {
    // We heard from the extension recently — it's alive on the
    // other end of the loopback.
    state = "connected";
  } else {
    // Server is up but no fresh heartbeat. This is the legitimate
    // "extension not installed / not activated yet" state and
    // also the "boot finished but no extension activation has
    // happened this session" state.
    state = "reconnecting";
  }
  return {
    state,
    publisherKind: "loopback_http",
    instanceJson: JSON.stringify({
      apiServerRunning: snap.apiServerRunning,
      apiServerPort: snap.apiServerPort,
      portFilePath: snap.portFilePath,
      lastExtensionContactAt: snap.lastExtensionContactAt,
      queuedPublishCount: snap.queuedPublishCount,
      reviewThreadCount: snap.reviewThreadCount,
    }),
    defaultThreadId,
    enabled,
  };
}

/**
 * Single-call helper that returns both the master enabled flag
 * and the per-project default thread id from the Rust bridge's
 * `kchatStatus()` snapshot. Issuing a single `kchatStatus()` call
 * for both fields is the only way to read them: every renderer-
 * facing handler (`kchat:status`, `kchat:reload`, `kchat:publish`,
 * `kchat:setEnabled`) needs both, and the N-API call + read-lock
 * acquire on the Rust side is the dominant cost. Doing it twice
 * (one round-trip per field) was a latency regression on the
 * 5 s status poll hot path that this helper closes.
 *
 * This variant FAILS OPEN. Returns `{ enabled: true,
 * defaultThreadId: null }` if the bridge is temporarily
 * unavailable (typical at boot — `kchat:status` is the very first
 * call the renderer issues): surfacing the error would hide the
 * loopback / extension status from the indicator chip even though
 * that signal is still meaningful. `enabled=true` matches
 * `KChatConfig::default` so the first poll doesn't flash a
 * disabled chip; `defaultThreadId=null` lets the renderer fall
 * back to its `kchat-default` constant.
 *
 * Use this ONLY on read-only display paths (`kchat:status`,
 * `kchat:reload`). Any write path or data-exposure path that gates
 * on `enabled` must use the strict variant below — defaulting to
 * `enabled=true` on a bridge blip silently overrides an explicit
 * user disable and would have been a real bug on the publish
 * gate (Phase 15 round-9 BUG_0001).
 */
export async function resolveEnabledAndDefaultThreadFromBridge(): Promise<{
  enabled: boolean;
  defaultThreadId: string | null;
}> {
  try {
    return await resolveEnabledAndDefaultThreadFromBridgeStrict();
  } catch (err) {
    console.warn(
      `[ipc] resolveEnabledAndDefaultThreadFromBridge: bridge kchat_status failed (${
        err instanceof Error ? err.message : String(err)
      }); defaulting enabled=true, defaultThreadId=null`,
    );
    return { enabled: true, defaultThreadId: null };
  }
}

/**
 * Strict single-call helper — same shape as the soft variant
 * above, but propagates bridge errors instead of swallowing them.
 *
 * Use this on any write path or data-exposure path that gates on
 * `enabled` (currently `kchat:publish` and `kchat:ingestReviews`).
 * If the bridge is unreachable we MUST NOT default to
 * `enabled=true`: doing so would silently override an explicit
 * user disable and let publishes through (or expose review
 * comments) during a bridge outage. Fail closed by propagating
 * the error; the renderer's PublishCardModal surfaces it inline
 * via `setError(msg)`, and the review panel treats it as a
 * polling error (stops the loop, surfaces a retry affordance).
 *
 * The error message intentionally includes the underlying cause
 * so a missing bridge.node / NAPI panic is recognisable in the
 * UI rather than masquerading as "disabled".
 */
export async function resolveEnabledAndDefaultThreadFromBridgeStrict(): Promise<{
  enabled: boolean;
  defaultThreadId: string | null;
}> {
  const status = await getBridge().kchatStatus();
  return {
    enabled: status.enabled,
    defaultThreadId: status.defaultThreadId ?? null,
  };
}

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
  // Phase 17 Group B Task 12. Persist a rendered PNG thumbnail
  // into the project's SQLCipher DB. We validate at the IPC boundary
  // *in addition to* the service-side magic-byte / dimension checks
  // so a renderer-side bug (forgot to encode, passed a string, etc.)
  // surfaces as a clean `IpcValidationError` instead of a NAPI type-
  // coercion failure that Sentry would log without the field name.
  ipcMain.handle(
    "project:setThumbnail",
    async (_e, { projectPath, png, width, height }) => {
      assertString(projectPath, "projectPath");
      assertUint8Array(png, "png");
      assertPositiveInteger(width, "width", 1, 4096);
      assertPositiveInteger(height, "height", 1, 4096);
      return getBridge().projectSetThumbnail(projectPath, png, width, height);
    },
  );
  ipcMain.handle("project:getThumbnail", async (_e, { projectPath }) => {
    assertString(projectPath, "projectPath");
    return getBridge().projectGetThumbnail(projectPath);
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
  // Phase 17 Group B Task 11. Both handlers run `assertObject`
  // against their parameter to enforce the bridge's object-shape
  // contract — a renderer-side bug that sends `undefined` /
  // `null` / a primitive on either argument surfaces as a typed
  // IPC error here rather than reaching the napi layer and
  // throwing a less-actionable napi-side type error.
  ipcMain.handle("design:listMaterials", async (_e, query) => {
    assertObject(query, "query");
    return getBridge().designListMaterials(
      query as Parameters<
        ReturnType<typeof getBridge>["designListMaterials"]
      >[0],
    );
  });
  // Object-arg shape (`{ materialId, update }`) matches every other
  // `design:*` handler above. The preload wraps the renderer's
  // positional (materialId, update) call into the object form so the
  // public `aec.design.updateMaterial(materialId, patch)` surface
  // stays unchanged.
  ipcMain.handle("design:updateMaterial", async (_e, params) => {
    assertObject(params, "params");
    const { materialId, update } = params as {
      materialId?: unknown;
      update?: unknown;
    };
    if (typeof materialId !== "string" || materialId.length === 0) {
      throw new Error("design:updateMaterial: materialId must be a string");
    }
    assertObject(update, "update");
    return getBridge().designUpdateMaterial(
      materialId,
      update as Parameters<
        ReturnType<typeof getBridge>["designUpdateMaterial"]
      >[1],
    );
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
  // service routes through `bim_read_schedule_rows`, which in turn
  // calls `aec_bim::xlsx_reader::read_xlsx_rows_with_header` (pure-
  // Rust OOXML reader, no native deps). This handler is a thin
  // wrapper that validates the `xlsxPath` and lets the service
  // surface read errors via the existing IPC error envelope.
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

  // ----- Phase 17 — output image read-back + SSIM compare -----
  ipcMain.handle("render:getOutputImage", async (_e, p) => {
    assertObject(p, "params");
    const jobId = (p as { jobId?: unknown }).jobId;
    assertString(jobId, "jobId");
    return getBridge().renderGetOutputImage({ jobId: jobId as string });
  });
  ipcMain.handle("render:compareSsim", async (_e, p) => {
    assertObject(p, "params");
    const aJobId = (p as { aJobId?: unknown }).aJobId;
    const bJobId = (p as { bJobId?: unknown }).bJobId;
    assertString(aJobId, "aJobId");
    assertString(bJobId, "bJobId");
    return getBridge().renderCompareSsim({
      aJobId: aJobId as string,
      bJobId: bJobId as string,
    });
  });
  ipcMain.handle("render:setEnvironmentMap", async (_e, p) => {
    assertObject(p, "params");
    const path = (p as { path?: unknown }).path;
    if (path !== null && typeof path !== "string") {
      throw new Error("renderSetEnvironmentMap: path must be string or null");
    }
    const intensity = (p as { intensity?: unknown }).intensity;
    if (intensity !== undefined && typeof intensity !== "number") {
      throw new Error("renderSetEnvironmentMap: intensity must be number");
    }
    return getBridge().renderSetEnvironmentMap({
      path: path as string | null,
      intensity: intensity as number | undefined,
    });
  });

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

  // ----- Extensions (Phase 16) -----
  //
  // Per-extension boot diagnostics. The bridge intentionally
  // degrades on per-extension load errors (a broken manifest must
  // not block the whole renderer from booting) but those silent
  // failures used to be invisible. This IPC exposes the Rust-side
  // buffered diagnostics so the Settings page can render a
  // read-only diagnostics card. Read-only and idempotent;
  // returns an empty array when nothing is broken (the renderer
  // treats that as the signal to hide the card entirely).
  ipcMain.handle("extensions:listLoadDiagnostics", async () =>
    getBridge().extensionsListLoadDiagnostics(),
  );

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
    //
    // We deliberately do NOT use `withResolvedProjectPath` here — that
    // helper throws `IpcValidationError` when no project is open, but
    // the proposal pack is allowed to degrade gracefully (cover text
    // just omits the project-specific counts). Same design choice the
    // `deliver:buildPack` handler made for the same reason.
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

  // ----- KChat (Phase 15: loopback HTTP + .kcz extension) -----
  //
  // Phase 12's socket / named-pipe transport is gone. The
  // Electron main process now hosts a loopback HTTP API on
  // 127.0.0.1 that the `.kcz` extension installed in KChat
  // Desktop talks to (see `kchat/kchatLocalApi.ts`). These IPC
  // handlers route the renderer-facing surface through the
  // Electron-side `kchatAppState` singleton; the Rust bridge's
  // KChat methods are kept for in-process tests / fallback only.
  ipcMain.handle("kchat:status", async () => {
    // Single bridge round-trip — see
    // `resolveEnabledAndDefaultThreadFromBridge`. Polled every
    // ~5 s by the renderer; splitting into two parallel calls
    // would double the N-API + RwLock acquisitions on the hot
    // path for no benefit since both fields come from the same
    // `kchatStatus()` payload.
    const { defaultThreadId, enabled } =
      await resolveEnabledAndDefaultThreadFromBridge();
    return buildKchatStatusResponse(defaultThreadId, enabled);
  });
  ipcMain.handle("kchat:reload", async () => {
    // In Phase 15 there is nothing to "reload" at the transport
    // layer — the loopback API has no upstream connection to
    // re-establish. Reload is now a synonym for "snapshot the
    // current status"; the renderer keeps the affordance for
    // backward compatibility (the Settings card's "Re-detect"
    // button maps to this). Critically, we must compute the
    // connection state with the SAME logic as `kchat:status`
    // (heartbeat-freshness based, not just "is the server up")
    // so that clicking "Re-detect" while the extension is
    // connected doesn't flash the indicator to `reconnecting`
    // until the next 5 s status poll corrects it.
    //
    // Same single-call optimisation as `kchat:status` above.
    const { defaultThreadId, enabled } =
      await resolveEnabledAndDefaultThreadFromBridge();
    return buildKchatStatusResponse(defaultThreadId, enabled);
  });
  ipcMain.handle("kchat:setEnabled", async (_e, params) => {
    // Settings page "Enable KChat integration" toggle. The bridge
    // persists the flag onto `KChatConfig::enabled`; subsequent
    // `kchat:publish` IPC calls observe it via the status payload
    // and refuse the publish *before* enqueueing into the
    // loopback queue when disabled. We return the fresh status
    // snapshot so the renderer can avoid a follow-up
    // `kchat:status` round trip.
    assertObject(params, "params");
    const enabled = (params as { enabled?: unknown }).enabled;
    if (typeof enabled !== "boolean") {
      throw new Error("kchatSetEnabled: enabled must be a boolean");
    }
    const bridgeStatus = await getBridge().kchatSetEnabled({ enabled });
    return buildKchatStatusResponse(
      bridgeStatus.defaultThreadId ?? null,
      bridgeStatus.enabled,
    );
  });
  ipcMain.handle("kchat:publish", async (_e, params) => {
    assertObject(params, "params");
    // Snapshot the bridge once for both the enabled gate and the
    // per-project default-thread lookup further down. Phase 12's
    // `KChatState::publish` checked `enabled` *before* touching
    // the transport; Phase 15's loopback queue lives outside the
    // Rust state, so the gate has to live here too. Refusing
    // publishes before they reach `enqueuePublish` is the
    // correct semantic: a queued card implies AEC Studio intends
    // to send it, and we don't intend anything when the user has
    // switched the integration off. The error is surfaced
    // verbatim in `PublishCardModal`'s inline error pane via
    // `setError(msg)`.
    //
    // FAIL CLOSED on bridge errors (round-9 BUG_0001 fix). The
    // soft helper defaults to `enabled=true` when the bridge is
    // unreachable, which is correct for the status indicator but
    // wrong for a write path: a transient bridge blip during a
    // publish would silently override an explicit user disable.
    // The strict variant propagates the underlying error so the
    // renderer surfaces it (and a retry rather than a silent
    // bypass-then-queue is the right UX).
    const bridgeKchat = await resolveEnabledAndDefaultThreadFromBridgeStrict();
    if (!bridgeKchat.enabled) {
      throw new Error(
        "kchatPublish: KChat integration is disabled \u2014 enable it in Settings before publishing.",
      );
    }
    const cardJson = (params as { cardJson?: unknown }).cardJson;
    assertString(cardJson, "cardJson");
    // The renderer's `PublishCardModal` posts the canonical
    // `ArtifactCard` shape mirrored from
    // `aec_core::kchat::ArtifactCard`
    // (artifact / caption / project_link / thumbnail_blake3 /
    // metadata). The local-queue `QueuedPublish` row needs a
    // different shape (cardId / threadId / body) so the .kcz
    // extension can drain it into KChat with a one-shot
    // `invokeProcedure("kchat.send_message")` call. We transform
    // at this boundary so neither side has to know about the
    // other's shape: the renderer keeps the wire-stable
    // ArtifactCard contract, and the loopback queue keeps the
    // QueuedPublish contract.
    let card: {
      artifact?: unknown;
      caption?: unknown;
      project_link?: unknown;
      thumbnail_blake3?: unknown;
      metadata?: unknown;
    };
    try {
      const parsed = JSON.parse(cardJson) as unknown;
      if (
        parsed === null ||
        typeof parsed !== "object" ||
        Array.isArray(parsed)
      ) {
        throw new Error(
          "kchatPublish: cardJson must encode an ArtifactCard object",
        );
      }
      card = parsed as typeof card;
    } catch (err) {
      if (err instanceof SyntaxError) {
        throw new Error(
          `kchatPublish: cardJson is not valid JSON (${err.message})`,
        );
      }
      throw err;
    }
    // The fields the renderer must always supply. Validate at the
    // IPC boundary so a misconfigured caller gets a descriptive
    // error rather than an opaque .kcz-extension failure later
    // on. These mirror `ArtifactCard::validate` on the Rust side.
    if (typeof card.artifact !== "string" || card.artifact.length === 0) {
      throw new Error("kchatPublish: cardJson.artifact is required");
    }
    if (typeof card.caption !== "string" || card.caption.trim().length === 0) {
      throw new Error("kchatPublish: cardJson.caption is required");
    }
    if (
      typeof card.project_link !== "string" ||
      !card.project_link.startsWith("aecstudio://")
    ) {
      throw new Error(
        "kchatPublish: cardJson.project_link must start with aecstudio://",
      );
    }
    // Resolve the per-project default thread the active project
    // has configured via its manifest's
    // `settings.kchat.default_thread_id`. The bridge tracks this
    // as `KChatConfig::default_thread_id`; when no project is
    // open or the project leaves the field unset we fall back to
    // `aec_core::DEFAULT_THREAD_ID` (`"kchat-default"`), matching
    // the renderer's `FALLBACK_THREAD_ID` constant and the
    // publisher-side default. The .kcz extension is free to map
    // this to a real KChat thread id when it acks via
    // POST /api/publish-to-thread. Reuses the bridge snapshot
    // taken above so we don't re-enter `kchatStatus()` for this
    // single handler invocation.
    const projectThread = bridgeKchat.defaultThreadId;
    const threadId =
      projectThread !== null && projectThread.length > 0
        ? projectThread
        : "kchat-default";
    // Mint a UUID v4 for the card id. The .kcz extension uses
    // it purely as a correlation handle; ordering is preserved
    // by the queue's append-only structure, not by the id.
    const cardId = randomUUID();
    // Body shown in the KChat thread. The renderer's caption
    // already includes any artifact-kind decoration the user
    // wants surfaced, so we forward it verbatim. The extension
    // gets the full ArtifactCard JSON via `cardJson` if it wants
    // to render richer markup (thumbnail link, project deeplink,
    // metadata chips).
    const body = card.caption;
    const queued = enqueuePublish({
      cardId,
      threadId,
      body,
      cardJson,
    });
    // Mirror the Phase 12 return shape so the renderer / tests
    // don't have to change. `messageId` is the cardId until the
    // extension reports the real KChat message id via
    // POST /api/publish-to-thread; the renderer uses the
    // messageId only as a correlation handle, never as a key.
    return {
      messageId: queued.cardId,
      threadId: queued.threadId,
      publishedAt: queued.queuedAt,
    };
  });
  ipcMain.handle("kchat:ingestReviews", async (_e, params) => {
    assertObject(params, "params");
    const threadId = (params as { threadId?: unknown }).threadId;
    assertString(threadId, "threadId");
    const rawSince = (params as { sinceIso?: unknown }).sinceIso;
    let sinceIso: string | null;
    if (rawSince === undefined || rawSince === null) {
      sinceIso = null;
    } else if (typeof rawSince === "string") {
      sinceIso = rawSince;
    } else {
      throw new Error("kchatIngestReviews: sinceIso must be a string or null");
    }
    // Phase 12 contract: the bridge's `kchat_ingest_reviews`
    // returned `KChatError::Disabled` when the master toggle was
    // off. Phase 15 moved the read out of the bridge and into the
    // Electron-side `kchatAppState` buffer, but the gate has to
    // survive the move — otherwise a disabled integration would
    // still hand out review-comment payloads to any caller that
    // bypassed the renderer's panel-level gate (extensions over
    // the loopback API, future scripted IPC). Fail closed on
    // bridge errors via the strict helper for the same reason
    // `kchat:publish` does (round-9 ANALYSIS_0002).
    const bridgeKchat = await resolveEnabledAndDefaultThreadFromBridgeStrict();
    if (!bridgeKchat.enabled) {
      throw new Error(
        "kchatIngestReviews: KChat integration is disabled \u2014 enable it in Settings before ingesting reviews.",
      );
    }
    const stored = getReviewCommentsForThread(threadId, sinceIso);
    // Map `StoredReviewComment` (the shape `kchatAppState.ts`
    // accumulates from the `.kcz` extension's
    // `POST /api/review-comments` calls) to `ReviewCommentRow` —
    // the shape `KChatReviewPanel.tsx` already decodes against.
    // The pure helper lives in `./kchat/reviewWireFormat` so the
    // renderer test suite can exercise the real wire-format
    // transform with a `ReviewCommentPayload[]` payload (closes
    // the round-5 ANALYSIS_0005 test-quality gap).
    const comments = mapStoredReviewCommentsToRows(stored);
    return {
      threadId,
      // Mirror the Phase 12 envelope (JSON-encoded arrays so the
      // renderer can keep its existing decoder). `cardsJson` is
      // empty in Phase 15 — review cards (extra metadata cards
      // attached to comments) are not yet emitted by the .kcz
      // extension.
      commentsJson: JSON.stringify(comments),
      cardsJson: "[]",
    };
  });
  ipcMain.handle("kchat:openInDesktop", async (_e, params) => {
    assertObject(params, "params");
    const url = (params as { url?: unknown }).url;
    assertString(url, "url");
    return openKchatDeeplink(url);
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
  // Phase 17 Task 21 — RGBA8 pixel readback for the renderer's
  // canvas paint loop. The bridge returns either a `{ bytes,
  // width, height, frameIndex }` payload (where `bytes` is a Node
  // Buffer wrapping a zero-copy view into the Rust Vec<u8>) or
  // `null` (= "viewport not sized yet" — the renderer falls back
  // to the unavailable overlay). No payload validation is required:
  // there are no input params and the bridge's return shape is
  // exhaustively pinned by the BridgeBackend / AecApi interfaces.
  ipcMain.handle("viewport:readFrameBuffer", async () =>
    getBridge().viewportReadFrameBuffer(),
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
 * Phase 17 Group B Task 12. The renderer hands the IPC layer a
 * `Uint8Array` of PNG bytes for `project:setThumbnail`. Electron
 * marshals typed arrays through structured clone, so we just need
 * to assert the right runtime shape on the main side. Node's
 * `Buffer` extends `Uint8Array`, so checking for the base class
 * accepts both.
 */
function assertUint8Array(
  value: unknown,
  field: string,
): asserts value is Uint8Array {
  if (!(value instanceof Uint8Array)) {
    throw new IpcValidationError(`${field} must be a Uint8Array`);
  }
  if (value.length === 0) {
    throw new IpcValidationError(`${field} must not be empty`);
  }
}

/**
 * Validate that `value` is a positive 32-bit integer within `[min,
 * max]` (inclusive). Used for thumbnail dimensions in Phase 17
 * Group B Task 12 to enforce sane viewport sizes before they hit
 * the SQL layer.
 */
function assertPositiveInteger(
  value: unknown,
  field: string,
  min: number,
  max: number,
): asserts value is number {
  if (
    typeof value !== "number" ||
    !Number.isFinite(value) ||
    !Number.isInteger(value) ||
    value < min ||
    value > max
  ) {
    throw new IpcValidationError(
      `${field} must be an integer in [${min}, ${max}] (got: ${String(value)})`,
    );
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
