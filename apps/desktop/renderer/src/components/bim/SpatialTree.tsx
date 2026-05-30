import { useState } from "react";

/**
 * One node of the BIM spatial hierarchy. The shape mirrors what the Rust
 * `aec_bim::Project` produces: a typed nesting of Site / Building / Storey /
 * Space, with leaf-level building elements (walls, doors, …) attached to
 * their containing storey or space. Each node carries a stable GUID so the
 * tree can be re-rendered without losing selection identity when the
 * underlying project graph mutates.
 */
export interface SpatialNode {
  id: string;
  kind:
    | "IfcProject"
    | "IfcSite"
    | "IfcBuilding"
    | "IfcBuildingStorey"
    | "IfcSpace"
    | "IfcElement";
  name: string;
  children?: SpatialNode[];
}

interface Props {
  root: SpatialNode | null;
  selectedId: string | null;
  /**
   * Called when the user clicks a row. Receives the row's `id` **and**
   * the mapped IFC `kind` so the page can derive classification badges
   * / property-editor schemas directly from the spatial graph without
   * having to hard-code id→class lookups. The kind comes from
   * `mapKind()` in `bim-spatial-tree.ts`, which mirrors
   * `crates/aec_bridge/src/bim_attach.rs` (IFC type names as the
   * `bim/spatial/<…>` suffix).
   */
  onSelect: (node: { id: string; kind: SpatialNode["kind"] }) => void;
}

export function SpatialTree({ root, selectedId, onSelect }: Props) {
  if (!root) {
    return (
      <aside
        className="bim-spatial bim-spatial--empty"
        data-testid="spatial-tree"
      >
        <p>No model loaded. Use "Import IFC" to load a project.</p>
      </aside>
    );
  }
  return (
    <aside
      className="bim-spatial"
      aria-label="Spatial hierarchy"
      data-testid="spatial-tree"
    >
      <SpatialNodeRow
        node={root}
        depth={0}
        selectedId={selectedId}
        onSelect={onSelect}
      />
    </aside>
  );
}

interface RowProps {
  node: SpatialNode;
  depth: number;
  selectedId: string | null;
  onSelect: (node: { id: string; kind: SpatialNode["kind"] }) => void;
}

function SpatialNodeRow({ node, depth, selectedId, onSelect }: RowProps) {
  const [expanded, setExpanded] = useState(depth < 3);
  const hasChildren = (node.children ?? []).length > 0;
  const isSelected = selectedId === node.id;
  return (
    <div className="bim-spatial__row">
      <button
        type="button"
        className={`bim-spatial__btn${isSelected ? " bim-spatial__btn--selected" : ""}`}
        style={{ paddingLeft: `${depth * 12 + 4}px` }}
        onClick={() => onSelect({ id: node.id, kind: node.kind })}
        data-testid={`spatial-node-${node.id}`}
      >
        {hasChildren && (
          <span
            className="bim-spatial__chevron"
            data-testid={`spatial-toggle-${node.id}`}
            onClick={(e) => {
              e.stopPropagation();
              setExpanded((x) => !x);
            }}
            role="button"
            aria-label={expanded ? "Collapse" : "Expand"}
          >
            {expanded ? "▾" : "▸"}
          </span>
        )}
        <span className="bim-spatial__kind">{shortKind(node.kind)}</span>
        <span className="bim-spatial__name">{node.name}</span>
      </button>
      {hasChildren && expanded && (
        <div className="bim-spatial__children">
          {node.children!.map((child) => (
            <SpatialNodeRow
              key={child.id}
              node={child}
              depth={depth + 1}
              selectedId={selectedId}
              onSelect={onSelect}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function shortKind(kind: SpatialNode["kind"]): string {
  switch (kind) {
    case "IfcProject":
      return "Project";
    case "IfcSite":
      return "Site";
    case "IfcBuilding":
      return "Bldg";
    case "IfcBuildingStorey":
      return "Lvl";
    case "IfcSpace":
      return "Spc";
    case "IfcElement":
      return "El";
  }
}
