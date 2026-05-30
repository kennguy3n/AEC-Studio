/**
 * Typed accessor for the AEC IPC bridge exposed by `preload.ts`.
 *
 * The preload sets `window.aec` with the typed `AecApi`. This module
 * re-exports a strongly typed `aec` that the renderer code calls.
 * Under Vitest the preload doesn't run, so we fall back to an
 * in-process backend that mirrors the real one (kept in
 * `electron/bridge.ts → inProcessBackend`).
 */

import type { AecApi } from "../../../electron/preload";
import { rendererInProcessBackend } from "./renderer-backend";

declare global {
  interface Window {
    aec?: AecApi;
  }
}

export const aec: AecApi = (() => {
  if (typeof window !== "undefined" && window.aec) {
    return window.aec;
  }
  // Vitest / SSR fallback.
  return rendererInProcessBackend();
})();

export type ProjectSummary = {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
};

/**
 * Phase 17 Group B Task 12. Renderer-side projection of a saved
 * project thumbnail blob. The `png` field is a `Uint8Array` (the
 * native bridge returns a Node `Buffer`, which subclasses
 * `Uint8Array`; the in-process fallback uses a plain typed array)
 * so the consumer can build a `Blob` URL via
 * `URL.createObjectURL(new Blob([png], { type: "image/png" }))`
 * without an intermediate base64 hop.
 */
export type ProjectThumbnail = {
  png: Uint8Array;
  width: number;
  height: number;
  updatedAt: string;
};

export type AssetSummary = {
  assetId: string;
  name: string;
  tags: string[];
  styleTags: string[];
  vendor: string | null;
  thumbnailDataUri: string | null;
};

/**
 * Phase 17 Group B Task 11. Renderer-side projection of the
 * bridge's `MaterialSummary`. Field shape must stay in lockstep with
 * `MaterialSummary` in `apps/desktop/electron/bridge.ts` and
 * `MaterialSummaryJs` in `crates/aec_bridge/src/napi_api.rs` —
 * drift surfaces as `undefined` on the `MaterialPanel` swatch
 * grid.
 *
 * `albedo` / `emissive` are linear-space `[r, g, b]` triples (each
 * channel in `[0.0, 1.0]`); the swatch view converts them to sRGB
 * + gamma-corrects when painting the PBR-style preview sphere.
 */
export type MaterialSummary = {
  materialId: string;
  name: string;
  albedo: [number, number, number];
  metallic: number;
  roughness: number;
  ior: number;
  transmission: number;
  emissive: [number, number, number];
  styleTags: string[];
  tags: string[];
};

export type MaterialListQuery = {
  search?: string;
  tags?: string[];
  styleTags?: string[];
  limit?: number;
};

export type MaterialUpdate = {
  albedo?: [number, number, number];
  metallic?: number;
  roughness?: number;
  ior?: number;
  transmission?: number;
  emissive?: [number, number, number];
};

export type RuntimeStatus = {
  tier: "Low" | "Medium" | "High" | "Pro";
  cpu: { model: string; physicalCores: number; logicalCores: number };
  ramTotalMb: number;
  ramAvailableMb: number;
  gpu: { vendor: string; model: string; vramMb: number } | null;
  os: string;
};

export type AiTool = {
  id: string;
  scope: string;
  maxEntitiesModified: number;
  description: string;
};

export type RenderJob = {
  jobId: string;
  status: "queued" | "running" | "completed" | "failed" | "cancelled";
  preset: string;
  progress: number;
  cameraId?: string | null;
  batchId?: string | null;
};

export type BatchProgress = {
  batchId: string;
  total: number;
  queued: number;
  running: number;
  completed: number;
  failed: number;
  cancelled: number;
  averageProgress: number;
};

export type MaterialFinding = {
  code: string;
  severity: "info" | "warning" | "error";
  materialId: string | null;
  message: string;
  fix: string | null;
};
