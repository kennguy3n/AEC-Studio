/**
 * Renderer-side helper for attaching a parsed IFC snapshot into the
 * active project's SQLCipher database.
 *
 * Attach is a follow-up to the `bim-import.ts` preview path: the user
 * picks an IFC, the renderer runs `importIfcWithSizeGuard` to preview
 * the file (entity counts, schema), and on confirm the renderer calls
 * `attachIfcToProject` to fold the snapshot into the project graph.
 *
 * The bridge's in-process snapshot cache means an import → attach
 * sequence on the same path does not re-parse the file: the second
 * call observes `parseCacheHit: true`.
 *
 * Like `bim-import.ts`, this helper returns a tagged-union outcome
 * rather than throwing — the renderer UX layer wants to surface a
 * toast, not crash an error boundary.
 */

import type { BimAttachSummary } from "../../../electron/bridge";
import { aec } from "./aec";

/**
 * Result of a `bim_attach_ifc` call.
 *
 *   * `"attached"` — the bridge committed the attach transaction and
 *     returned a `BimAttachSummary` (post-dedup counts).
 *   * `"failed"` — the bridge returned an error (file missing,
 *     malformed IFC, project DB locked, ...). `error` is the raw
 *     error message so the renderer can surface it in a toast.
 */
export type BimAttachOutcome =
  | { kind: "attached"; result: BimAttachSummary }
  | { kind: "failed"; error: string };

/**
 * Attach a parsed IFC snapshot to a project.
 *
 *   * `projectPath` — absolute path to the `.aecstudio` project
 *     package. The renderer can pass `ProjectSummary.path` from a
 *     recents entry verbatim.
 *   * `ifcPath` — absolute path to the IFC file. If the renderer
 *     just ran `importIfcWithSizeGuard(ifcPath)`, this should be the
 *     same string so the snapshot cache hits. Canonicalisation is
 *     done bridge-side.
 *
 * The bridge handles the dedup contract: re-attaching the same file
 * with identical content reports `_unchanged` counts (not
 * `_inserted` / `_updated`). The renderer should display the
 * `_inserted` / `_updated` / `_unchanged` triplet in a status line
 * so the user can tell incremental imports apart from no-ops.
 */
export async function attachIfcToProject(
  projectPath: string,
  ifcPath: string,
): Promise<BimAttachOutcome> {
  try {
    const result = await aec.bim.attachIfc(projectPath, ifcPath);
    return { kind: "attached", result };
  } catch (err) {
    return { kind: "failed", error: errorMessage(err) };
  }
}

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return "unknown error";
  }
}
