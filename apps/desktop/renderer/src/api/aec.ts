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

export type AssetSummary = {
  assetId: string;
  name: string;
  tags: string[];
  styleTags: string[];
  vendor: string | null;
  thumbnailDataUri: string | null;
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
