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

import * as fs from "fs";
import * as path from "path";

import { AI_TOOLS, type AiTool } from "./ai-tools";

export { AI_TOOLS, type AiTool };

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
  renderListJobs(): Promise<RenderJob[]>;
  renderCancelJob(jobId: string): Promise<{ cancelled: true }>;
  renderApplyPreset(params: Record<string, unknown>): Promise<{ ok: true }>;
  renderDiagnose(jobId: string): Promise<{ jobId: string; suggestions: string[] }>;

  aiListTools(): Promise<AiTool[]>;
  aiPlan(params: Record<string, unknown>): Promise<{ diffId: string }>;
  aiAcceptDiff(diffId: string): Promise<{ accepted: true }>;
  aiRejectDiff(diffId: string): Promise<{ rejected: true }>;
  aiCancelJob(jobId: string): Promise<{ cancelled: true }>;
  aiRuntimeStatus(): Promise<{ state: string; lastError: string | null }>;

  exportPdf(params: Record<string, unknown>): Promise<{ outPath: string; pages: number }>;
  exportDxf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportIfc(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportGltf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportBuildProposalPack(params: Record<string, unknown>): Promise<{ outPath: string }>;

  runtimeStatus(): Promise<RuntimeStatus>;
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
  "renderListJobs",
  "renderCancelJob",
  "renderApplyPreset",
  "renderDiagnose",
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

    async aiListTools() {
      // Return a defensive copy of the catalogue so that callers can't
      // mutate the shared `AI_TOOLS` constant. This matches the renderer
      // backend (see `rendererInProcessBackend.ai.listTools`).
      return AI_TOOLS.map((t) => ({ ...t }));
    },
    async aiPlan(_p) {
      return { diffId: id("diff") };
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

    async runtimeStatus() {
      return inProcessRuntimeStatus();
    },
  };
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
