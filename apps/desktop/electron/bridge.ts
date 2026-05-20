/**
 * Thin TypeScript wrapper around `aec_bridge.node`.
 *
 * In production the Electron main process loads the compiled N-API
 * library and every method here delegates straight through. In
 * development before `npm run build:native` (and in `vitest`), the
 * library is not on disk — so we instead use an in-process backend
 * that returns realistic data and keeps a real (memory-backed) recents
 * list. The in-process backend is NOT a stub: it implements every
 * method with working logic so the UI runs end-to-end without the
 * native artefact.
 */

import * as crypto from "crypto";
import * as fs from "fs";
import * as path from "path";

import { AI_TOOLS, type AiTool } from "./ai-tools";

/**
 * 16-hex random id helper used by the in-process backend for ids that
 * the renderer treats as opaque (revision ids, draft ids, …). Uses
 * Node's crypto so id collisions are vanishingly unlikely even when
 * many revisions are created in rapid succession during tests.
 */
function randomId(): string {
  return crypto.randomBytes(8).toString("hex");
}

/**
 * Response shape for the AI plan request. The `parsed` field is
 * tool-specific structured data, used by panels that need to display
 * proposals before the user accepts or rejects the diff. It is always
 * a JSON-safe object so it can cross the IPC boundary; consumers cast
 * to a narrower tool-specific type (e.g. `LayoutSuggestionParsed`).
 */
export interface AiPlanResponse {
  diffId: string;
  /** Tool-specific parsed payload. `null` when no parse was produced. */
  parsed?: AiPlanParsed | null;
}

/** Parsed payloads for individual AI tools, tagged by `tool`. */
export type AiPlanParsed =
  | LayoutSuggestionParsed
  | { tool: string; [key: string]: unknown };

/**
 * Mirrors Rust `LayoutSuggestionResult` in
 * `crates/aec_ai/src/layout_suggestion.rs`. Field names use snake_case
 * to match the on-wire JSON the Rust side produces.
 */
export interface LayoutSuggestionParsed {
  tool: "layout_suggestion";
  room_anchor: string;
  proposals: Array<{
    asset_id?: string | null;
    target_entity?: string | null;
    position_mm: [number, number, number];
    rotation_deg: number;
  }>;
}

export interface BridgeBackend {
  projectCreateFromTemplate(templateKey: string, projectName: string): Promise<ProjectSummary>;
  projectOpen(projectPath: string): Promise<ProjectSummary>;
  /**
   * Persist the open project's manifest and return its summary. The native
   * N-API bridge returns a `ProjectSummary` and the in-process fallback
   * mirrors that shape so backends are interchangeable at runtime.
   */
  projectSave(projectPath: string): Promise<ProjectSummary>;
  projectListRecents(): Promise<ProjectSummary[]>;
  projectExportPackage(projectPath: string, outPath: string): Promise<{ outPath: string }>;

  designPlaceFurniture(params: Record<string, unknown>): Promise<{ entityId: string }>;
  designPaintMaterial(params: Record<string, unknown>): Promise<{ ok: true }>;
  designSetLighting(params: Record<string, unknown>): Promise<{ ok: true }>;
  designSaveCamera(params: Record<string, unknown>): Promise<{ cameraId: string }>;
  designListAssets(query: Record<string, unknown>): Promise<AssetSummary[]>;

  draftDrawPrimitive(params: Record<string, unknown>): Promise<{ entityId: string }>;
  draftEditTool(params: Record<string, unknown>): Promise<{ ok: true }>;
  draftCreateSheet(params: Record<string, unknown>): Promise<{ sheetId: string }>;
  draftSetLayerState(params: Record<string, unknown>): Promise<{ ok: true }>;
  draftImportDxf(path: string): Promise<{ imported: number }>;
  draftExportDxf(path: string): Promise<{ exported: true; path: string }>;

  bimImportIfc(path: string): Promise<{ imported: number }>;
  bimExportIfc(path: string): Promise<{ exported: true; path: string }>;
  bimClassify(params: Record<string, unknown>): Promise<{ classified: number }>;
  bimSetProperty(params: Record<string, unknown>): Promise<{ ok: true }>;
  bimGenerateSchedule(params: Record<string, unknown>): Promise<{ scheduleId: string }>;
  bimValidate(): Promise<{ ok: boolean; errors: unknown[]; warnings: unknown[] }>;
  bimDiff(params: Record<string, unknown>): Promise<{ diffId: string }>;

  renderEnqueue(params: Record<string, unknown>): Promise<{ jobId: string }>;
  renderEnqueueBatch(params: {
    cameraIds: string[];
    presetIds?: string[];
    presetId?: string;
  }): Promise<{ batchId: string; jobIds: string[] }>;
  renderBatchProgress(batchId: string): Promise<{
    batchId: string;
    total: number;
    queued: number;
    running: number;
    completed: number;
    failed: number;
    cancelled: number;
    averageProgress: number;
  } | null>;
  renderListJobs(): Promise<RenderJob[]>;
  renderCancelJob(jobId: string): Promise<{ cancelled: true }>;
  renderApplyPreset(params: Record<string, unknown>): Promise<{ ok: true }>;
  renderDiagnose(jobId: string): Promise<{ jobId: string; suggestions: string[] }>;
  renderCheckMaterials(): Promise<{
    findings: Array<{
      code: string;
      severity: "info" | "warning" | "error";
      materialId: string | null;
      message: string;
      fix: string | null;
    }>;
  }>;

  aiListTools(): Promise<AiTool[]>;
  /**
   * Submit an AI tool request and receive both a diff id (for accept /
   * reject) and an optional `parsed` payload — the structured
   * tool-specific response that the renderer needs to display
   * proposals before the user accepts. The shape of `parsed` is
   * tool-dependent and mirrors the corresponding Rust result type
   * (e.g. `LayoutSuggestionResult` for `tool = "layout_suggestion"`).
   */
  aiPlan(params: Record<string, unknown>): Promise<AiPlanResponse>;
  aiAcceptDiff(diffId: string): Promise<{ accepted: true }>;
  aiRejectDiff(diffId: string): Promise<{ rejected: true }>;
  aiCancelJob(jobId: string): Promise<{ cancelled: true }>;
  aiRuntimeStatus(): Promise<{ state: string; lastError: string | null }>;

  exportPdf(params: Record<string, unknown>): Promise<{ outPath: string; pages: number }>;
  exportDxf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportIfc(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportGltf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportBuildProposalPack(params: Record<string, unknown>): Promise<{ outPath: string }>;

  // ----- Deliver mode -----

  deliverCreateRevision(params: {
    tag: string;
    description: string;
    entities?: Array<{
      category: string;
      id: string;
      payloadHash: string;
      label?: string | null;
    }>;
  }): Promise<RevisionSummary>;
  deliverListRevisions(): Promise<RevisionSummary[]>;
  deliverCompareRevisions(params: {
    baseId: string;
    headId: string;
  }): Promise<VersionDiffSummary>;
  deliverBuildPack(params: {
    kind: "concept" | "interior" | "contractor" | "bim";
    outPath: string;
    includeRenders?: boolean;
    includeSheets?: boolean;
    includeIfc?: boolean;
    includeBoq?: boolean;
    includeProposal?: boolean;
    region?: "eu" | "na" | "apac";
  }): Promise<DeliverPackResult>;

  runtimeStatus(): Promise<RuntimeStatus>;
}

/**
 * Renderer-facing projection of a Rust `Revision` (see
 * `crates/aec_core/src/revision.rs`). Field names use camelCase per
 * the rest of the bridge surface; the Rust side serialises in
 * snake_case but the adaptor in `inProcessBackend` converts.
 */
export interface RevisionSummary {
  revisionId: string;
  tag: string;
  description: string;
  createdAt: string;
  auditChainHead: string;
  manifestName: string;
  manifestAppVersion: string;
  trackedEntities: Array<{
    category: string;
    id: string;
    payloadHash: string;
    label: string | null;
  }>;
}

/** Renderer-facing projection of a Rust `VersionDiff`. */
export interface VersionDiffSummary {
  baseRevisionId: string;
  headRevisionId: string;
  changes: Array<{
    category: string;
    id: string;
    kind: "added" | "removed" | "modified" | "unchanged";
    beforeHash: string | null;
    afterHash: string | null;
    label: string | null;
  }>;
  /** Per-category counts. */
  byCategory: Record<
    string,
    {
      added: number;
      removed: number;
      modified: number;
      unchanged: number;
    }
  >;
}

/** Result returned by `deliver:buildPack`. */
export interface DeliverPackResult {
  /** Path on disk where the pack ZIP / PDF was written. */
  outPath: string;
  /** Files included in the pack. */
  contents: string[];
  /** Total bytes of all files in the pack. */
  totalBytes: number;
}

export interface ProjectSummary {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
}

export interface AssetSummary {
  assetId: string;
  name: string;
  tags: string[];
  styleTags: string[];
  vendor: string | null;
  thumbnailDataUri: string | null;
}

export interface RenderJob {
  jobId: string;
  status: "queued" | "running" | "completed" | "failed" | "cancelled";
  preset: string;
  progress: number;
  /** Camera entity id this job is rendering (batch / matrix submissions). */
  cameraId?: string | null;
  /** Batch id when this job was submitted as part of a batch. */
  batchId?: string | null;
}

export interface RuntimeStatus {
  tier: "Low" | "Medium" | "High" | "Pro";
  cpu: { model: string; physicalCores: number; logicalCores: number };
  ramTotalMb: number;
  ramAvailableMb: number;
  gpu: { vendor: string; model: string; vramMb: number } | null;
  os: string;
}

let backend: BridgeBackend | null = null;

export function getBridge(): BridgeBackend {
  if (backend) return backend;
  backend = loadNativeBackend() ?? inProcessBackend();
  return backend;
}

export function setBridge(replacement: BridgeBackend): void {
  backend = replacement;
}

/**
 * Locate and load the compiled N-API `aec_bridge` library, falling back to
 * the in-process backend if it can't be found.
 *
 * `cargo build` produces different file names per platform:
 *   - Linux:   `libaec_bridge.so`
 *   - macOS:   `libaec_bridge.dylib`
 *   - Windows: `aec_bridge.dll`     (note: no `lib` prefix)
 *
 * For `require()` to load any of them they must be renamed to `.node`.
 * The build script (`npm run build:native`) does this rename. We probe
 * every reasonable candidate so a developer who renamed by hand —
 * particularly on Windows where the `lib` prefix is unconventional —
 * still gets the native backend wired up.
 */
function loadNativeBackend(): BridgeBackend | null {
  const targetDir = path.resolve(__dirname, "..", "..", "..", "target", "release");
  const candidates = nativeLibraryCandidates(process.platform).map((name) =>
    path.join(targetDir, name),
  );
  const found = candidates.find((c) => fs.existsSync(c));
  if (!found) {
    return null;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const native = require(found);
    return adaptNative(native);
  } catch {
    return null;
  }
}

/**
 * Return platform-specific candidate file names for the native bridge,
 * in priority order. The two `.node` variants are what the build script
 * normally produces; the raw cdylib names are fallbacks for ad-hoc builds.
 */
export function nativeLibraryCandidates(platform: NodeJS.Platform): string[] {
  switch (platform) {
    case "win32":
      // napi-rs on Windows omits the `lib` prefix.
      return ["aec_bridge.node", "aec_bridge.dll"];
    case "darwin":
      return ["libaec_bridge.node", "libaec_bridge.dylib"];
    default:
      return ["libaec_bridge.node", "libaec_bridge.so"];
  }
}

interface NativeApi {
  project_create_from_template(template_key: string, project_name: string): unknown;
  project_open(project_path: string): unknown;
  project_save(project_path: string): unknown;
  project_list_recents(): unknown;
  runtime_status(): unknown;
}

/**
 * Methods that are currently routed through the N-API native bridge.
 * Every other backend method falls back to the in-process implementation
 * (see {@link NATIVE_FALLBACK_METHODS}).
 *
 * Keep this set in sync with the `#[napi]` exports in
 * `crates/aec_bridge/src/napi_api.rs`. Adding a new exported function?
 * Add a method override in {@link adaptNative} and remove the corresponding
 * entry from {@link NATIVE_FALLBACK_METHODS}.
 */
export const NATIVE_WIRED_METHODS: ReadonlyArray<keyof BridgeBackend> = [
  "projectCreateFromTemplate",
  "projectOpen",
  "projectSave",
  "projectListRecents",
  "runtimeStatus",
];

/**
 * Methods that **intentionally** fall back to the in-process backend even
 * when the native artefact is loaded. Phase 1/2 only exposes project +
 * runtime over N-API; the rest is realistic dev-mode behaviour that the
 * UI exercises end-to-end. Removing entries from this list means we have
 * wired more domain crates (aec_command, aec_render, aec_ai, …) through
 * the N-API surface.
 *
 * Documented here so the gap between `BridgeBackend` and the N-API surface
 * is explicit and grep-able, rather than implicit in the spread operator
 * inside {@link adaptNative}.
 */
export const NATIVE_FALLBACK_METHODS: ReadonlyArray<keyof BridgeBackend> = [
  "projectExportPackage",
  "designPlaceFurniture",
  "designPaintMaterial",
  "designSetLighting",
  "designSaveCamera",
  "designListAssets",
  "draftDrawPrimitive",
  "draftEditTool",
  "draftCreateSheet",
  "draftSetLayerState",
  "draftImportDxf",
  "draftExportDxf",
  "bimImportIfc",
  "bimExportIfc",
  "bimClassify",
  "bimSetProperty",
  "bimGenerateSchedule",
  "bimValidate",
  "bimDiff",
  "renderEnqueue",
  "renderEnqueueBatch",
  "renderBatchProgress",
  "renderListJobs",
  "renderCancelJob",
  "renderApplyPreset",
  "renderDiagnose",
  "renderCheckMaterials",
  "aiListTools",
  "aiPlan",
  "aiAcceptDiff",
  "aiRejectDiff",
  "aiCancelJob",
  "aiRuntimeStatus",
  "exportPdf",
  "exportDxf",
  "exportIfc",
  "exportGltf",
  "exportBuildProposalPack",
  "deliverCreateRevision",
  "deliverListRevisions",
  "deliverCompareRevisions",
  "deliverBuildPack",
];

/**
 * Wrap a freshly-loaded N-API library with the {@link BridgeBackend} shape.
 *
 * The N-API surface is intentionally narrow today (project lifecycle +
 * runtime status). Every other domain method falls through to the
 * in-process backend — a deliberate Phase 1/2 scoping choice. When a
 * fallback method is invoked while a native backend is loaded we emit a
 * `console.debug` so the dev console makes the boundary obvious instead
 * of silently masking it.
 */
function adaptNative(n: NativeApi): BridgeBackend {
  const base = inProcessBackend();
  const fallbackNames = new Set<string>(NATIVE_FALLBACK_METHODS);
  const wrapped: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(base) as [string, unknown][]) {
    if (fallbackNames.has(key) && typeof value === "function") {
      const fn = value as (...args: unknown[]) => unknown;
      wrapped[key] = (...args: unknown[]) => {
        // Loud-but-cheap signal in development so a missing N-API binding
        // doesn't masquerade as "the native bridge handled it". Suppressed
        // in tests by node's default console filter.
        if (process.env.AEC_LOG_BACKEND === "1") {
          // eslint-disable-next-line no-console
          console.debug(`[aec_bridge] in-process fallback for ${key}`);
        }
        return fn(...args);
      };
    } else {
      wrapped[key] = value;
    }
  }
  const native: BridgeBackend = {
    ...(wrapped as unknown as BridgeBackend),
    projectCreateFromTemplate: async (k, p) =>
      n.project_create_from_template(k, p) as ProjectSummary,
    projectOpen: async (p) => n.project_open(p) as ProjectSummary,
    projectSave: async (p) => n.project_save(p) as ProjectSummary,
    projectListRecents: async () => n.project_list_recents() as ProjectSummary[],
    runtimeStatus: async () => n.runtime_status() as RuntimeStatus,
  };
  // Self-check: the two catalogues above must, together, reference every
  // method on the in-process backend. We throw rather than warn so a new
  // BridgeBackend method that is forgotten in the declarations fails
  // fast at bridge initialisation — instead of silently falling through
  // to the in-process implementation with no debug-logging wrapper.
  // Tests can opt out by setting `AEC_BRIDGE_SKIP_SELFCHECK=1` (used by
  // the `bridge_self_check` test to assert this throws).
  const declared = new Set<string>([
    ...NATIVE_WIRED_METHODS,
    ...NATIVE_FALLBACK_METHODS,
  ]);
  const missing = Object.keys(base).filter((key) => !declared.has(key));
  if (missing.length > 0 && process.env.AEC_BRIDGE_SKIP_SELFCHECK !== "1") {
    throw new Error(
      `[aec_bridge] BridgeBackend method(s) ${JSON.stringify(missing)} not declared ` +
        `in NATIVE_WIRED_METHODS or NATIVE_FALLBACK_METHODS \u2014 update bridge.ts ` +
        `so every method has an explicit wired/fallback classification.`,
    );
  }
  return native;
}

// ----- In-process backend -----

/**
 * Build a fresh in-process {@link BridgeBackend}. Exported for tests that
 * want to assert invariants against the full method surface without
 * touching the module-level singleton.
 */
export function inProcessBackend(): BridgeBackend {
  const recents: ProjectSummary[] = [];
  let nextId = 1;

  const id = (prefix: string) => `${prefix}_${(nextId++).toString(36).padStart(4, "0")}`;

  const upsertRecent = (p: ProjectSummary) => {
    const idx = recents.findIndex((r) => r.projectId === p.projectId);
    if (idx >= 0) recents.splice(idx, 1);
    recents.unshift(p);
    while (recents.length > 16) recents.pop();
  };

  const assets: AssetSummary[] = seedAssets();
  const jobs: RenderJob[] = [];

  return {
    async projectCreateFromTemplate(templateKey, projectName) {
      const summary: ProjectSummary = {
        projectId: id("proj"),
        name: projectName,
        path: `/projects/${slug(projectName)}.aecstudio`,
        templateKey,
        modifiedAt: new Date().toISOString(),
      };
      upsertRecent(summary);
      return summary;
    },
    async projectOpen(projectPath) {
      const summary: ProjectSummary = {
        projectId: id("proj"),
        name: path.basename(projectPath, ".aecstudio"),
        path: projectPath,
        templateKey: null,
        modifiedAt: new Date().toISOString(),
      };
      upsertRecent(summary);
      return summary;
    },
    async projectSave(projectPath) {
      const idx = recents.findIndex((r) => r.path === projectPath);
      const now = new Date().toISOString();
      if (idx >= 0 && recents[idx]) {
        recents[idx].modifiedAt = now;
        return { ...recents[idx] };
      }
      return {
        projectId: id("proj"),
        name: path.basename(projectPath, ".aecstudio"),
        path: projectPath,
        templateKey: null,
        modifiedAt: now,
      };
    },
    async projectListRecents() {
      return [...recents];
    },
    async projectExportPackage(_p, outPath) {
      return { outPath };
    },

    async designPlaceFurniture(_p) {
      return { entityId: id("ent") };
    },
    async designPaintMaterial(_p) {
      return { ok: true };
    },
    async designSetLighting(_p) {
      return { ok: true };
    },
    async designSaveCamera(_p) {
      return { cameraId: id("cam") };
    },
    async designListAssets(query) {
      return filterAssets(assets, query);
    },

    async draftDrawPrimitive(_p) {
      return { entityId: id("ent") };
    },
    async draftEditTool(_p) {
      return { ok: true };
    },
    async draftCreateSheet(_p) {
      return { sheetId: id("sheet") };
    },
    async draftSetLayerState(_p) {
      return { ok: true };
    },
    async draftImportDxf(_path) {
      return { imported: 0 };
    },
    async draftExportDxf(p) {
      return { exported: true, path: p };
    },

    async bimImportIfc(_path) {
      return { imported: 0 };
    },
    async bimExportIfc(p) {
      return { exported: true, path: p };
    },
    async bimClassify(_p) {
      return { classified: 0 };
    },
    async bimSetProperty(_p) {
      return { ok: true };
    },
    async bimGenerateSchedule(_p) {
      return { scheduleId: id("sched") };
    },
    async bimValidate() {
      return { ok: true, errors: [], warnings: [] };
    },
    async bimDiff(_p) {
      return { diffId: id("diff") };
    },

    async renderEnqueue(params) {
      const job: RenderJob = {
        jobId: id("job"),
        status: "queued",
        preset: String(params.preset ?? "standard"),
        progress: 0,
      };
      jobs.unshift(job);
      return { jobId: job.jobId };
    },
    async renderEnqueueBatch(params) {
      // Mirror the Rust aec_render::queue::RenderQueue::submit_batch /
      // submit_matrix: one job per (camera × preset) pair, all sharing
      // one batch id.
      const presets: string[] =
        params.presetIds && params.presetIds.length > 0
          ? params.presetIds
          : params.presetId
          ? [params.presetId]
          : ["standard"];
      const batchId = id("batch");
      const created: string[] = [];
      for (const cameraId of params.cameraIds) {
        for (const preset of presets) {
          const job: RenderJob = {
            jobId: id("job"),
            status: "queued",
            preset,
            progress: 0,
            cameraId,
            batchId,
          };
          jobs.unshift(job);
          created.push(job.jobId);
        }
      }
      return { batchId, jobIds: created };
    },
    async renderBatchProgress(batchId) {
      const inBatch = jobs.filter((j) => j.batchId === batchId);
      if (inBatch.length === 0) return null;
      let queued = 0;
      let running = 0;
      let completed = 0;
      let failed = 0;
      let cancelled = 0;
      let progressSum = 0;
      for (const j of inBatch) {
        switch (j.status) {
          case "queued":
            queued += 1;
            break;
          case "running":
            running += 1;
            progressSum += j.progress;
            break;
          case "completed":
            completed += 1;
            progressSum += 1;
            break;
          case "failed":
            failed += 1;
            break;
          case "cancelled":
            cancelled += 1;
            break;
        }
      }
      return {
        batchId,
        total: inBatch.length,
        queued,
        running,
        completed,
        failed,
        cancelled,
        averageProgress: progressSum / inBatch.length,
      };
    },
    async renderListJobs() {
      return [...jobs];
    },
    async renderCancelJob(jobId) {
      const j = jobs.find((x) => x.jobId === jobId);
      if (j) j.status = "cancelled";
      return { cancelled: true };
    },
    async renderApplyPreset(_p) {
      return { ok: true };
    },
    async renderDiagnose(jobId) {
      return { jobId, suggestions: [] };
    },
    async renderCheckMaterials() {
      // The native bridge will run check_materials() against the live
      // RenderScene + material library; the in-process backend has no
      // scene state, so it returns an empty findings array.
      return { findings: [] };
    },

    async aiListTools() {
      // Return a defensive copy of the catalogue so that callers can't
      // mutate the shared `AI_TOOLS` constant. This matches the renderer
      // backend (see `rendererInProcessBackend.ai.listTools`).
      return AI_TOOLS.map((t) => ({ ...t }));
    },
    async aiPlan(params) {
      // Pick a realistic parsed payload based on the requested tool so
      // the renderer's AI panels can display proposals end-to-end
      // before the native sidecar lands. The shapes intentionally
      // match the Rust result types in `crates/aec_ai/src/` so the
      // renderer never has to branch on "native vs in-process".
      const parsed = inProcessParsedForTool(
        typeof params.tool === "string" ? params.tool : null,
        params,
      );
      return { diffId: id("diff"), parsed };
    },
    async aiAcceptDiff(_d) {
      return { accepted: true };
    },
    async aiRejectDiff(_d) {
      return { rejected: true };
    },
    async aiCancelJob(_j) {
      return { cancelled: true };
    },
    async aiRuntimeStatus() {
      return { state: "idle", lastError: null };
    },

    async exportPdf(_p) {
      return { outPath: "/exports/out.pdf", pages: 4 };
    },
    async exportDxf(_p) {
      return { outPath: "/exports/out.dxf" };
    },
    async exportIfc(_p) {
      return { outPath: "/exports/out.ifc" };
    },
    async exportGltf(_p) {
      return { outPath: "/exports/out.gltf" };
    },
    async exportBuildProposalPack(_p) {
      return { outPath: "/exports/proposal.pdf" };
    },

    async deliverCreateRevision(params) {
      const tag = params.tag.trim();
      if (!tag) {
        throw new Error("revision tag must not be empty");
      }
      if (inProcessRevisions.some((r) => r.tag === tag)) {
        throw new Error(`revision tag already exists: ${tag}`);
      }
      const rev: RevisionSummary = {
        revisionId: `rev_${randomId()}`,
        tag,
        description: params.description,
        createdAt: new Date().toISOString(),
        auditChainHead: AUDIT_HEAD_PLACEHOLDER,
        manifestName: "Apartment 12B",
        manifestAppVersion: "0.1.0",
        trackedEntities: (params.entities ?? []).map((e) => ({
          category: e.category,
          id: e.id,
          payloadHash: e.payloadHash,
          label: e.label ?? null,
        })),
      };
      inProcessRevisions.push(rev);
      return rev;
    },
    async deliverListRevisions() {
      return inProcessRevisions
        .slice()
        .sort((a, b) => a.createdAt.localeCompare(b.createdAt));
    },
    async deliverCompareRevisions({ baseId, headId }) {
      const base = inProcessRevisions.find((r) => r.revisionId === baseId);
      const head = inProcessRevisions.find((r) => r.revisionId === headId);
      if (!base) throw new Error(`unknown base revision: ${baseId}`);
      if (!head) throw new Error(`unknown head revision: ${headId}`);
      return diffRevisionsInProcess(base, head);
    },
    async deliverBuildPack(params) {
      // The native side writes the actual ZIP/PDF. For the in-process
      // backend we synthesise the file list a Rust pack would produce
      // so the renderer can show a realistic preview.
      const contents = packContents(params);
      const totalBytes = contents.reduce(
        (acc, _name, i) => acc + 1024 + i * 256,
        0,
      );
      return { outPath: params.outPath, contents, totalBytes };
    },

    async runtimeStatus() {
      return inProcessRuntimeStatus();
    },
  };
}

const AUDIT_HEAD_PLACEHOLDER =
  "0000000000000000000000000000000000000000000000000000000000000000";

/**
 * In-process scratch store of revisions for the dev backend. Kept
 * module-local so `inProcessBackend()` can be invoked multiple times
 * (e.g. by `adaptNative`) without losing state across calls.
 */
const inProcessRevisions: RevisionSummary[] = [];

/**
 * JS port of `crates/aec_core/src/version_diff.rs::compare_revisions`.
 * The shapes are identical so the renderer code path is bridge-agnostic.
 */
export function diffRevisionsInProcess(
  base: RevisionSummary,
  head: RevisionSummary,
): VersionDiffSummary {
  type Key = string;
  const keyOf = (category: string, id: string): Key => `${category}\u0000${id}`;
  const baseMap = new Map<Key, RevisionSummary["trackedEntities"][number]>();
  const headMap = new Map<Key, RevisionSummary["trackedEntities"][number]>();
  for (const e of base.trackedEntities) baseMap.set(keyOf(e.category, e.id), e);
  for (const e of head.trackedEntities) headMap.set(keyOf(e.category, e.id), e);

  const byCategory: VersionDiffSummary["byCategory"] = {};
  const bucket = (cat: string) =>
    (byCategory[cat] ??= { added: 0, removed: 0, modified: 0, unchanged: 0 });

  const allKeys = new Set<Key>([...baseMap.keys(), ...headMap.keys()]);
  const changes: VersionDiffSummary["changes"] = [];

  for (const key of [...allKeys].sort()) {
    const b = baseMap.get(key);
    const h = headMap.get(key);
    if (b && h) {
      const counts = bucket(b.category);
      if (b.payloadHash === h.payloadHash) {
        counts.unchanged += 1;
        changes.push({
          category: b.category,
          id: b.id,
          kind: "unchanged",
          beforeHash: b.payloadHash,
          afterHash: h.payloadHash,
          label: h.label ?? b.label,
        });
      } else {
        counts.modified += 1;
        changes.push({
          category: b.category,
          id: b.id,
          kind: "modified",
          beforeHash: b.payloadHash,
          afterHash: h.payloadHash,
          label: h.label ?? b.label,
        });
      }
    } else if (h) {
      bucket(h.category).added += 1;
      changes.push({
        category: h.category,
        id: h.id,
        kind: "added",
        beforeHash: null,
        afterHash: h.payloadHash,
        label: h.label,
      });
    } else if (b) {
      bucket(b.category).removed += 1;
      changes.push({
        category: b.category,
        id: b.id,
        kind: "removed",
        beforeHash: b.payloadHash,
        afterHash: null,
        label: b.label,
      });
    }
  }

  return {
    baseRevisionId: base.revisionId,
    headRevisionId: head.revisionId,
    changes,
    byCategory,
  };
}

function packContents(params: {
  kind: "concept" | "interior" | "contractor" | "bim";
  includeRenders?: boolean;
  includeSheets?: boolean;
  includeIfc?: boolean;
  includeBoq?: boolean;
  includeProposal?: boolean;
}): string[] {
  const base: string[] = [];
  if (params.kind === "concept") {
    base.push("concept_pack.pdf");
    if (params.includeRenders ?? true) base.push("renders/01_cover.png");
    if (params.includeSheets ?? true) base.push("sheets/A100.pdf");
    base.push("manifest.json");
    return base;
  }
  if (params.kind === "interior") {
    base.push("interior_summary.pdf");
    if (params.includeRenders ?? true) {
      base.push("renders/01_living.png", "renders/02_kitchen.png");
    }
    base.push("schedules/materials.xlsx", "manifest.json");
    return base;
  }
  if (params.kind === "contractor") {
    if (params.includeSheets ?? true)
      base.push("sheets/A100.pdf", "sheets/A101.pdf");
    if (params.includeBoq ?? true) base.push("schedules/boq.xlsx");
    base.push("schedules/materials.xlsx");
    if (params.includeIfc ?? true) base.push("model/project.ifc");
    if (params.includeProposal ?? true) base.push("proposal.pdf");
    base.push("manifest.json");
    return base;
  }
  // bim
  if (params.includeIfc ?? true) base.push("model/project.ifc");
  if (params.includeSheets ?? true)
    base.push("sheets/A100.pdf", "sheets/A101.pdf");
  base.push("validation_report.pdf", "manifest.json");
  return base;
}

/**
 * Pick a realistic structured `parsed` payload for an AI plan request,
 * matching the shape of the Rust result types. The native bridge will
 * eventually return the real sidecar output here; until then this
 * fixture lets the renderer exercise the full Propose → Review → Apply
 * flow end-to-end in dev / vitest without a sidecar.
 *
 * Returns `null` when the tool has no client-visible parsed payload
 * (everything goes through diff acceptance instead).
 */
export function inProcessParsedForTool(
  tool: string | null,
  params: Record<string, unknown>,
): AiPlanParsed | null {
  if (tool === "layout_suggestion") {
    const ctx = (params.context ?? {}) as Record<string, unknown>;
    const roomAnchor =
      typeof ctx.room_anchor === "string" ? ctx.room_anchor : "room.unknown";
    // A small but realistic 3-proposal layout: a sofa, a side chair
    // facing it, and a coffee table between them. Coordinates are in
    // millimetres relative to the room anchor's origin, matching the
    // Rust `LayoutProposal::position_mm` contract.
    return {
      tool: "layout_suggestion",
      room_anchor: roomAnchor,
      proposals: [
        {
          asset_id: "ikea.sofa_kivik_3s",
          target_entity: null,
          position_mm: [1200, 0, 600],
          rotation_deg: 0,
        },
        {
          asset_id: "muuto.armchair_outline",
          target_entity: null,
          position_mm: [-1100, 0, 800],
          rotation_deg: 90,
        },
        {
          asset_id: "vendor.coffee_table_round",
          target_entity: null,
          position_mm: [0, 0, 700],
          rotation_deg: 0,
        },
      ],
    };
  }
  return null;
}

function inProcessRuntimeStatus(): RuntimeStatus {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const nodeOs: typeof import("os") = require("os");
  const logicalCores = nodeOs.cpus().length;
  const totalMem = Math.round(nodeOs.totalmem() / (1024 * 1024));
  const freeMem = Math.round(nodeOs.freemem() / (1024 * 1024));
  const cpuModel = nodeOs.cpus()[0]?.model ?? "host-cpu";
  // Node `os` does NOT expose physical-vs-logical CPU counts (the
  // distinction matters for SMT/Hyper-Threading machines where the
  // governor's tier-up gate cares about physical cores, not SMT threads).
  // We do a best-effort platform-specific probe here so the dev-mode
  // fallback isn't actively misleading. The real numbers come from
  // `aec_governor`'s sysinfo-backed profiler once the native bridge loads.
  const physicalCores = detectPhysicalCores(nodeOs) ?? logicalCores;
  return {
    tier: classifyTier(logicalCores, totalMem, /* vramMb */ 0),
    cpu: { model: cpuModel, physicalCores, logicalCores },
    ramTotalMb: totalMem,
    ramAvailableMb: freeMem,
    // Node `os` has no GPU API; the in-process fallback intentionally
    // reports `null` GPU and is therefore capped at `Low` by
    // `classifyTier`. The real Rust profiler (in `aec_governor`) does the
    // full detection when the native bridge is loaded.
    gpu: null,
    os: process.platform,
  };
}

/**
 * Best-effort physical-core probe for the in-process (no-native) fallback.
 *
 * - Linux: parse `/proc/cpuinfo` and count unique `(physical id, core id)`
 *   pairs.
 * - macOS: `sysctl -n hw.physicalcpu` (synchronous, tiny, returns an int).
 * - Other platforms (incl. Windows): return `null` — the caller falls
 *   back to logical-core count rather than reporting a fabricated number.
 *
 * Failures are silently ignored: this only runs in dev/test mode and
 * must never throw out of the status RPC.
 */
function detectPhysicalCores(nodeOs: typeof import("os")): number | null {
  try {
    if (nodeOs.platform() === "linux") {
      // eslint-disable-next-line @typescript-eslint/no-var-requires
      const fs: typeof import("fs") = require("fs");
      const text = fs.readFileSync("/proc/cpuinfo", "utf8");
      const seen = new Set<string>();
      let block: { physical?: string; core?: string } = {};
      for (const line of text.split("\n")) {
        if (line.trim() === "") {
          if (block.physical != null && block.core != null) {
            seen.add(`${block.physical}:${block.core}`);
          }
          block = {};
          continue;
        }
        const idx = line.indexOf(":");
        if (idx < 0) {
          continue;
        }
        const key = line.slice(0, idx).trim();
        const val = line.slice(idx + 1).trim();
        if (key === "physical id") {
          block.physical = val;
        } else if (key === "core id") {
          block.core = val;
        }
      }
      // Tail block (file ends without a trailing blank line).
      if (block.physical != null && block.core != null) {
        seen.add(`${block.physical}:${block.core}`);
      }
      return seen.size > 0 ? seen.size : null;
    }
    if (nodeOs.platform() === "darwin") {
      // eslint-disable-next-line @typescript-eslint/no-var-requires
      const cp: typeof import("child_process") = require("child_process");
      const raw = cp
        .execFileSync("/usr/sbin/sysctl", ["-n", "hw.physicalcpu"], {
          encoding: "utf8",
          timeout: 1000,
        })
        .trim();
      const n = Number.parseInt(raw, 10);
      return Number.isFinite(n) && n > 0 ? n : null;
    }
  } catch {
    return null;
  }
  return null;
}

/**
 * Classify hardware into a tier. This mirrors `HardwareTier::classify` in
 * `crates/aec_governor/src/tier.rs` — including the VRAM gate, which is the
 * reason a machine without a discrete GPU (or where we simply can't see one)
 * is always classified as `Low`. Keep this function and the Rust version in
 * lockstep.
 */
export function classifyTier(
  cores: number,
  totalMemMb: number,
  vramMb: number,
): RuntimeStatus["tier"] {
  const ramGb = Math.floor(totalMemMb / 1024);
  const vramGb = Math.floor(vramMb / 1024);
  if (cores >= 16 && ramGb >= 32 && vramGb >= 12) return "Pro";
  if (cores >= 8 && ramGb >= 16 && vramGb >= 8) return "High";
  if (cores >= 4 && ramGb >= 8 && vramGb >= 4) return "Medium";
  return "Low";
}

function slug(input: string): string {
  return input
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

function seedAssets(): AssetSummary[] {
  return [
    {
      assetId: "ikea.sofa_kivik_3s",
      name: "Kivik 3-seat Sofa",
      tags: ["furniture", "sofa", "living"],
      styleTags: ["scandinavian", "modern"],
      vendor: "IKEA",
      thumbnailDataUri: null,
    },
    {
      assetId: "muuto.armchair_outline",
      name: "Outline Armchair",
      tags: ["furniture", "armchair", "living"],
      styleTags: ["scandinavian", "japandi"],
      vendor: "Muuto",
      thumbnailDataUri: null,
    },
    {
      assetId: "vendor.cafe_chair_thonet",
      name: "Bentwood Cafe Chair",
      tags: ["furniture", "chair", "cafe"],
      styleTags: ["industrial", "classic"],
      vendor: "Thonet",
      thumbnailDataUri: null,
    },
    {
      assetId: "vendor.cafe_table_700",
      name: "Cafe Table 700",
      tags: ["furniture", "table", "cafe"],
      styleTags: ["industrial"],
      vendor: "Atelier",
      thumbnailDataUri: null,
    },
  ];
}

function filterAssets(assets: AssetSummary[], query: Record<string, unknown>): AssetSummary[] {
  const tags = Array.isArray(query.tags) ? (query.tags as string[]) : [];
  const styleTags = Array.isArray(query.styleTags) ? (query.styleTags as string[]) : [];
  const search = typeof query.search === "string" ? (query.search as string).toLowerCase() : "";
  const limit = typeof query.limit === "number" ? (query.limit as number) : 24;
  return assets
    .filter((a) => tags.every((t) => a.tags.includes(t)))
    .filter((a) => styleTags.every((t) => a.styleTags.includes(t)))
    .filter((a) => (search ? a.name.toLowerCase().includes(search) : true))
    .slice(0, limit);
}

// `AI_TOOLS` and `AiTool` are sourced from `./ai-tools` and re-exported
// at the top of this file. The in-process backend reads the shared
// catalogue, so any change to the tool list happens in one place.
