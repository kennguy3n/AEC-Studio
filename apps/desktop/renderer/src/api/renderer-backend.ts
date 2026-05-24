/**
 * Renderer-side fallback backend used by Vitest. Mirrors the
 * in-process backend that the Electron main process uses when the
 * native bridge artefact is absent.
 */

import type { AecApi } from "../../../electron/preload";
import { AI_TOOLS } from "../../../electron/ai-tools";
import {
  BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
  classifyTier,
  diffRevisionsInProcess,
  inProcessParsedForTool,
  type RevisionSummary,
  type VersionDiffSummary,
} from "../../../electron/bridge";

interface Recent {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
}

export function rendererInProcessBackend(): AecApi {
  const recents: Recent[] = [];
  let nextId = 1;
  const newId = (prefix: string) =>
    `${prefix}_${(nextId++).toString(36).padStart(4, "0")}`;
  const upsert = (r: Recent) => {
    const i = recents.findIndex((x) => x.projectId === r.projectId);
    if (i >= 0) recents.splice(i, 1);
    recents.unshift(r);
    while (recents.length > 16) recents.pop();
  };

  const assets = [
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

  return {
    project: {
      createFromTemplate: async (templateKey, projectName) => {
        const summary: Recent = {
          projectId: newId("proj"),
          name: projectName,
          path: `/projects/${projectName.toLowerCase().replace(/\W+/g, "_")}.aecstudio`,
          templateKey,
          modifiedAt: new Date().toISOString(),
        };
        upsert(summary);
        return summary;
      },
      open: async (projectPath) => {
        const summary: Recent = {
          projectId: newId("proj"),
          name: projectPath.split("/").pop()?.replace(".aecstudio", "") ?? "Project",
          path: projectPath,
          templateKey: null,
          modifiedAt: new Date().toISOString(),
        };
        upsert(summary);
        return summary;
      },
      save: async (projectPath) => {
        const idx = recents.findIndex((r) => r.path === projectPath);
        const now = new Date().toISOString();
        if (idx >= 0 && recents[idx]) {
          recents[idx].modifiedAt = now;
          return { ...recents[idx] };
        }
        return {
          projectId: newId("proj"),
          name: projectPath.split("/").pop()?.replace(".aecstudio", "") ?? "Project",
          path: projectPath,
          templateKey: null,
          modifiedAt: now,
        };
      },
      listRecents: async () => [...recents],
      exportPackage: async (_p, outPath) => ({ outPath }),
    },
    design: {
      placeFurniture: async () => ({ entityId: newId("ent") }),
      paintMaterial: async () => ({ ok: true }),
      setLighting: async () => ({ ok: true }),
      saveCamera: async () => ({ cameraId: newId("cam") }),
      listAssets: async (query) => {
        const q = query as { tags?: string[]; styleTags?: string[]; search?: string; limit?: number };
        return assets
          .filter((a) => (q.tags ?? []).every((t) => a.tags.includes(t)))
          .filter((a) => (q.styleTags ?? []).every((t) => a.styleTags.includes(t)))
          .filter((a) =>
            q.search ? a.name.toLowerCase().includes(q.search.toLowerCase()) : true,
          )
          .slice(0, q.limit ?? 24);
      },
    },
    draft: {
      drawPrimitive: async () => ({ entityId: newId("ent") }),
      editTool: async () => ({ ok: true }),
      createSheet: async () => ({ sheetId: newId("sheet") }),
      setLayerState: async () => ({ ok: true }),
      importDxf: async () => ({ imported: 0 }),
      exportDxf: async (p) => ({ exported: true, path: p }),
    },
    bim: {
      importIfc: async () => ({ imported: 0 }),
      checkFileSize: async (path) => ({
        path,
        fileSizeBytes: 0,
        largeFileWarning: false,
        thresholdBytes: BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
      }),
      exportIfc: async (p) => ({ exported: true, path: p }),
      classify: async () => ({ classified: 0 }),
      setProperty: async () => ({ ok: true }),
      generateSchedule: async () => ({ scheduleId: newId("sched") }),
      validate: async () => ({ ok: true, errors: [], warnings: [] }),
      diff: async () => ({ diffId: newId("diff") }),
    },
    render: {
      enqueueRender: async () => ({ jobId: newId("job") }),
      listJobs: async () => [],
      cancelJob: async () => ({ cancelled: true }),
      applyPreset: async () => ({ ok: true }),
      diagnose: async (jobId) => ({ jobId, suggestions: [] }),
      enqueueBatch: async (params) => {
        const presets =
          (params.presetIds && params.presetIds.length > 0
            ? params.presetIds
            : params.presetId
              ? [params.presetId]
              : ["standard"]);
        const batchId = newId("batch");
        const jobIds: string[] = [];
        for (let i = 0; i < params.cameraIds.length * presets.length; i++) {
          jobIds.push(newId("job"));
        }
        return { batchId, jobIds };
      },
      batchProgress: async (batchId) => ({
        batchId,
        total: 0,
        queued: 0,
        running: 0,
        completed: 0,
        failed: 0,
        cancelled: 0,
        averageProgress: 0,
      }),
      checkMaterials: async () => ({ findings: [] }),
    },
    ai: {
      // Return a defensive copy so callers (and Vitest harnesses) can't
      // mutate the shared catalogue.
      listTools: async () => AI_TOOLS.map((t) => ({ ...t })),
      plan: async (params: Record<string, unknown>) => {
        const tool = typeof params.tool === "string" ? params.tool : null;
        const parsed = inProcessParsedForTool(tool, params);
        return { diffId: newId("diff"), parsed };
      },
      acceptDiff: async () => ({ accepted: true }),
      rejectDiff: async () => ({ rejected: true }),
      cancelJob: async () => ({ cancelled: true }),
      runtimeStatus: async () => ({ state: "idle", lastError: null }),
    },
    export: {
      exportPdf: async () => ({ outPath: "/exports/out.pdf", pages: 4 }),
      exportDxf: async () => ({ outPath: "/exports/out.dxf" }),
      exportIfc: async () => ({ outPath: "/exports/out.ifc" }),
      exportGltf: async () => ({ outPath: "/exports/out.gltf" }),
      buildProposalPack: async () => ({ outPath: "/exports/proposal.pdf" }),
    },
    deliver: deliverMock(newId),
    runtime: {
      // Derive the tier from the same `classifyTier` the production
      // backend uses so the test fixture cannot drift away from real
      // classifier behaviour. The fixture deliberately includes a small
      // discrete GPU so the Medium-tier code path is exercised end-to-end.
      status: async () => ({
        tier: classifyTier(
          /* cores */ 4,
          /* totalMemMb */ 16384,
          /* vramMb */ 4096,
        ),
        cpu: { model: "test-cpu", physicalCores: 4, logicalCores: 8 },
        ramTotalMb: 16384,
        ramAvailableMb: 8192,
        gpu: { vendor: "test-gpu", model: "test-mid", vramMb: 4096 },
        os: "test",
      }),
    },
  };
}

/**
 * Renderer-side fixture for the `deliver` IPC namespace. Mirrors the
 * shape of `apps/desktop/electron/preload.ts` `deliver` so vitest tests
 * can exercise revision + pack composer flows without spinning up the
 * Electron host. The diff helper reuses `diffRevisionsInProcess` from
 * the bridge module so the fixture cannot drift away from production
 * comparison semantics.
 */
function deliverMock(newId: (prefix: string) => string) {
  const revisions: RevisionSummary[] = [];

  return {
    async createRevision(params: {
      tag: string;
      description: string;
      entities?: Array<{
        category: string;
        id: string;
        payloadHash: string;
        label?: string | null;
      }>;
    }): Promise<RevisionSummary> {
      const tag = params.tag.trim();
      if (!tag) throw new Error("revision tag must not be empty");
      if (revisions.some((r) => r.tag === tag)) {
        throw new Error(`revision tag already exists: ${tag}`);
      }
      const rev: RevisionSummary = {
        revisionId: newId("rev"),
        tag,
        description: params.description,
        createdAt: new Date().toISOString(),
        auditChainHead:
          "0000000000000000000000000000000000000000000000000000000000000000",
        manifestName: "Apartment 12B",
        manifestAppVersion: "0.1.0",
        trackedEntities: (params.entities ?? []).map((e) => ({
          category: e.category,
          id: e.id,
          payloadHash: e.payloadHash,
          label: e.label ?? null,
        })),
      };
      revisions.push(rev);
      return rev;
    },
    async listRevisions(): Promise<RevisionSummary[]> {
      return revisions
        .slice()
        .sort((a, b) => a.createdAt.localeCompare(b.createdAt));
    },
    async compareRevisions(params: {
      baseId: string;
      headId: string;
    }): Promise<VersionDiffSummary> {
      const base = revisions.find((r) => r.revisionId === params.baseId);
      const head = revisions.find((r) => r.revisionId === params.headId);
      if (!base) throw new Error(`unknown base revision: ${params.baseId}`);
      if (!head) throw new Error(`unknown head revision: ${params.headId}`);
      return diffRevisionsInProcess(base, head);
    },
    async buildPack(params: {
      kind: "concept" | "interior" | "contractor" | "bim";
      outPath: string;
      includeRenders?: boolean;
      includeSheets?: boolean;
      includeIfc?: boolean;
      includeBoq?: boolean;
      includeProposal?: boolean;
      region?: "eu" | "na" | "apac";
    }): Promise<{ outPath: string; contents: string[]; totalBytes: number }> {
      const contents: string[] = [];
      if (params.kind === "concept") {
        contents.push("concept_pack.pdf", "manifest.json");
      } else if (params.kind === "interior") {
        contents.push(
          "interior_summary.pdf",
          "schedules/materials.xlsx",
          "manifest.json",
        );
      } else if (params.kind === "contractor") {
        contents.push("sheets/A100.pdf", "schedules/boq.xlsx", "manifest.json");
      } else {
        contents.push(
          "model/project.ifc",
          "validation_report.pdf",
          "manifest.json",
        );
      }
      return {
        outPath: params.outPath,
        contents,
        totalBytes: contents.length * 4096,
      };
    },
  };
}
