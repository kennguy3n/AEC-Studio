/**
 * Phase 17 Group C Task 16 — convert the persisted project graph
 * (a flat list of `EntityRecord`s with `id` / `kind` / `parent` /
 * `body`) into the hierarchical [`SpatialNode`] tree the
 * [`SpatialTree`] component renders.
 *
 * The project graph rows are written by `attach_snapshot`
 * (`crates/aec_bridge/src/bim_attach.rs`) every time an IFC file is
 * imported / re-attached. Each spatial-hierarchy node lands as
 * `entities` row with:
 *
 *   `kind`     = `"bim/spatial/IfcProject"` / `"…/IfcSite"` / etc.
 *   `parent`   = the containing spatial node's `EntityId`
 *                (NULL for the project root)
 *   `body`     = serde-JSON of `BimSpatialBody`
 *                (`ifc_guid`, `ifc_class`, `name`, …)
 *
 * Building elements (walls / slabs / …) land with `kind` =
 * `"bim/element/<IfcClass>"` and `parent` set to the containing
 * spatial node. We surface a small number of elements per spatial
 * node so the user sees real geometry in the tree, but do NOT
 * recursively expand the entire element graph (it would explode the
 * tree size on a typical MEP federation — 12 000+ elements per
 * project — and the user navigates elements via the viewport
 * selection picking, not the spatial-tree drill-down).
 */

import type { EntityRecord } from "./commands";
import type { SpatialNode } from "../components/bim/SpatialTree";

const SPATIAL_PREFIX = "bim/spatial/";
const ELEMENT_PREFIX = "bim/element/";

/**
 * Maximum number of building elements surfaced under a single
 * spatial node in the tree view. The full element list is still
 * reachable via the viewport / property editor — this cap exists so
 * the tree itself stays scannable on dense MEP / federation models.
 */
const MAX_ELEMENTS_PER_SPATIAL = 24;

interface PartialBody {
  name?: unknown;
  ifc_class?: unknown;
}

/**
 * Build a [`SpatialNode`] tree from a flat `EntityRecord[]`.
 *
 * Returns `null` when the project has no spatial entities yet
 * (fresh project, no IFC imported). The caller should fall back to
 * a tree-component empty state in that case — never to fake demo
 * data, per the Phase 17 Group C user-journey-completeness
 * directive: we want the user to see "Import an IFC to populate
 * this view", not a built-in fake "Building A / L1 / Living"
 * placeholder that could be mistaken for real project data.
 */
export function buildSpatialTree(rows: EntityRecord[]): SpatialNode | null {
  const spatial = rows.filter((r) => r.kind.startsWith(SPATIAL_PREFIX));
  if (spatial.length === 0) return null;

  // Map every spatial entity id to its node so we can attach
  // children as we walk the list.
  const nodes = new Map<string, SpatialNode>();
  for (const row of spatial) {
    const body = (row.body ?? {}) as PartialBody;
    nodes.set(row.id, {
      id: row.id,
      kind: mapKind(row.kind),
      name: deriveName(row, body),
      children: [],
    });
  }

  // Link children to their parents. Spatial rows whose `parent` is
  // null become candidate roots (typically exactly one — the
  // `IfcProject`).
  const roots: SpatialNode[] = [];
  for (const row of spatial) {
    const node = nodes.get(row.id);
    if (!node) continue;
    if (row.parent && nodes.has(row.parent)) {
      const parent = nodes.get(row.parent)!;
      (parent.children ??= []).push(node);
    } else {
      roots.push(node);
    }
  }

  // Attach a capped slice of building elements under each spatial
  // node so the user sees real geometry leaves in the tree. The
  // cap prevents pathological MEP federations from blowing the
  // tree size out — full element navigation is via the viewport
  // selection path, not the tree drill-down.
  for (const row of rows) {
    if (!row.kind.startsWith(ELEMENT_PREFIX)) continue;
    if (!row.parent) continue;
    const parent = nodes.get(row.parent);
    if (!parent) continue;
    parent.children ??= [];
    const currentElements = parent.children.filter(
      (c) => c.kind === "IfcElement",
    );
    if (currentElements.length >= MAX_ELEMENTS_PER_SPATIAL) continue;
    const body = (row.body ?? {}) as PartialBody;
    parent.children.push({
      id: row.id,
      kind: "IfcElement",
      name: deriveName(row, body),
      children: [],
    });
  }

  // Sort each spatial node's children so spatial sub-nodes come
  // before elements (mirrors the way users read the hierarchy:
  // location first, then contents) and within each group preserve
  // the BFS insertion order from the bridge.
  for (const node of nodes.values()) {
    if (!node.children) continue;
    node.children.sort(
      (a, b) =>
        spatialPriority(a.kind) - spatialPriority(b.kind),
    );
  }

  if (roots.length === 1) return roots[0];
  if (roots.length === 0) return null;

  // Multi-root: synthesise a parent so the SpatialTree component
  // (single-root contract) can render. Multi-root is rare in
  // practice — only IFC federation snapshots without a unifying
  // IfcProject row produce it — but we don't want to silently drop
  // any spatial nodes.
  return {
    id: "__synthetic_root__",
    kind: "IfcProject",
    name: "Project",
    children: roots,
  };
}

function mapKind(rawKind: string): SpatialNode["kind"] {
  const suffix = rawKind.slice(SPATIAL_PREFIX.length);
  switch (suffix) {
    case "IfcProject":
      return "IfcProject";
    case "IfcSite":
      return "IfcSite";
    case "IfcBuilding":
      return "IfcBuilding";
    case "IfcBuildingStorey":
      return "IfcBuildingStorey";
    case "IfcSpace":
      return "IfcSpace";
    default:
      // Unknown spatial subclass — render as `IfcElement` so the
      // tree node still appears with a meaningful badge instead of
      // being dropped. Future schema additions (IFC5 sub-regions,
      // etc.) hit this branch until the renderer-side mapping is
      // taught the new kind.
      return "IfcElement";
  }
}

function deriveName(row: EntityRecord, body: PartialBody): string {
  if (typeof body.name === "string" && body.name.length > 0) {
    return body.name;
  }
  if (typeof body.ifc_class === "string" && body.ifc_class.length > 0) {
    return body.ifc_class;
  }
  // Fall back to the entity id so the user can still distinguish
  // sibling nodes even when the IFC source omitted names.
  return row.id;
}

function spatialPriority(kind: SpatialNode["kind"]): number {
  switch (kind) {
    case "IfcProject":
      return 0;
    case "IfcSite":
      return 1;
    case "IfcBuilding":
      return 2;
    case "IfcBuildingStorey":
      return 3;
    case "IfcSpace":
      return 4;
    case "IfcElement":
      // Elements always sort after spatial sub-nodes.
      return 100;
  }
}
