/**
 * Renderer-side fallback backend used by Vitest. Mirrors the
 * in-process backend that the Electron main process uses when the
 * native bridge artefact is absent.
 */

import type { AecApi } from "../../../electron/preload";

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
    },
    ai: {
      listTools: async () => [
        {
          id: "plan_detection",
          scope: "design,draft,bim",
          maxEntitiesModified: 200,
          description: "Detect walls and openings from imported plan.",
        },
        {
          id: "style_assistant",
          scope: "design",
          maxEntitiesModified: 50,
          description: "Propose furniture and finishes for a given style brief.",
        },
      ],
      plan: async () => ({ diffId: newId("diff") }),
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
    runtime: {
      status: async () => ({
        tier: "Medium" as const,
        cpu: { model: "test-cpu", physicalCores: 4, logicalCores: 8 },
        ramTotalMb: 16384,
        ramAvailableMb: 8192,
        gpu: null,
        os: "test",
      }),
    },
  };
}
