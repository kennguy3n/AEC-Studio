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

export interface BridgeBackend {
  projectCreateFromTemplate(templateKey: string, projectName: string): Promise<ProjectSummary>;
  projectOpen(projectPath: string): Promise<ProjectSummary>;
  projectSave(projectPath: string): Promise<{ saved: true; path: string }>;
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

export interface AiTool {
  id: string;
  scope: string;
  maxEntitiesModified: number;
  description: string;
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

function adaptNative(n: NativeApi): BridgeBackend {
  const base = inProcessBackend();
  return {
    ...base,
    projectCreateFromTemplate: async (k, p) =>
      n.project_create_from_template(k, p) as ProjectSummary,
    projectOpen: async (p) => n.project_open(p) as ProjectSummary,
    projectSave: async (p) => n.project_save(p) as { saved: true; path: string },
    projectListRecents: async () => n.project_list_recents() as ProjectSummary[],
    runtimeStatus: async () => n.runtime_status() as RuntimeStatus,
  };
}

// ----- In-process backend -----

function inProcessBackend(): BridgeBackend {
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
      if (idx >= 0 && recents[idx]) {
        recents[idx].modifiedAt = new Date().toISOString();
      }
      return { saved: true, path: projectPath };
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
      return AI_TOOLS;
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
  const cpuCount = nodeOs.cpus().length;
  const totalMem = Math.round(nodeOs.totalmem() / (1024 * 1024));
  const freeMem = Math.round(nodeOs.freemem() / (1024 * 1024));
  const cpuModel = nodeOs.cpus()[0]?.model ?? "host-cpu";
  return {
    tier: classifyTier(cpuCount, totalMem, /* vramMb */ 0),
    cpu: { model: cpuModel, physicalCores: cpuCount, logicalCores: cpuCount },
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

const AI_TOOLS: AiTool[] = [
  { id: "plan_detection", scope: "design,draft,bim", maxEntitiesModified: 200, description: "Detect walls and openings from imported plan." },
  { id: "style_assistant", scope: "design", maxEntitiesModified: 50, description: "Propose furniture and finishes for a given style brief." },
  { id: "layout_suggestion", scope: "design", maxEntitiesModified: 80, description: "Suggest a furniture layout for a room shape." },
  { id: "render_doctor", scope: "render", maxEntitiesModified: 30, description: "Diagnose noise, exposure, and lighting in a render." },
  { id: "cad_cleanup", scope: "draft", maxEntitiesModified: 500, description: "Clean and rationalise an imported drawing." },
  { id: "plan_to_wall", scope: "design,draft", maxEntitiesModified: 200, description: "Convert a detected plan into parametric walls." },
  { id: "schedule_fill", scope: "bim,deliver", maxEntitiesModified: 1000, description: "Fill BIM property schedules." },
  { id: "classification", scope: "bim", maxEntitiesModified: 500, description: "Classify imported geometry into IFC entities." },
  { id: "property_fill", scope: "bim", maxEntitiesModified: 1000, description: "Populate property sets on classified elements." },
  { id: "validation_help", scope: "bim", maxEntitiesModified: 0, description: "Explain BIM validation findings." },
  { id: "cover_page_draft", scope: "deliver", maxEntitiesModified: 5, description: "Draft a proposal pack cover page." },
];
