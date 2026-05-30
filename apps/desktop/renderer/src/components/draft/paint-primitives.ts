/**
 * Phase 17 Group C Task 20 — paint imported DXF entities into the
 * Draft mode's 2D canvas.
 *
 * The bridge stores every DXF entity as an `EntityRecord` with
 * `kind === "primitive"` and a `body` matching the Rust
 * `aec_command::commands::draft::DrawPrimitive` struct:
 *
 *   {
 *     entity_id: string,
 *     primitive: {
 *       kind: "line" | "polyline" | "arc" | "circle" |
 *             "ellipse" | "spline" | "hatch" | "text" | "m_text",
 *       layer: string,
 *       …shape-specific fields
 *     }
 *   }
 *
 * The painter walks every primitive record, computes the bounding
 * box, fits the world→screen transform to the canvas, then paints
 * each visible primitive (filtered by the `visibleLayers` set).
 *
 * Coordinates from the bridge are CAD-style: Y increases up. The
 * painter flips Y so the canvas top-left is (0, 0) and the world
 * origin sits at the canvas center (offset by the bounding box
 * mean).
 *
 * Unsupported primitive variants (`spline`, `ellipse`, `hatch`)
 * fall back to a polyline approximation when one can be derived;
 * `m_text` falls back to plain text. This keeps the user from
 * seeing an empty canvas after importing a real DXF file even if
 * the painter doesn't yet have a dedicated renderer for every
 * variant.
 */

import type { EntityRecord } from "../../api/commands";

type Vec2 = [number, number];

interface PolylineVertex {
  at: Vec2;
  bulge?: number;
}

interface LinePrim {
  kind: "line";
  layer: string;
  start: Vec2;
  end: Vec2;
}

interface PolylinePrim {
  kind: "polyline";
  layer: string;
  vertices: PolylineVertex[];
  closed: boolean;
}

interface ArcPrim {
  kind: "arc";
  layer: string;
  center: Vec2;
  radius: number;
  start_angle: number;
  end_angle: number;
}

interface CirclePrim {
  kind: "circle";
  layer: string;
  center: Vec2;
  radius: number;
}

interface EllipsePrim {
  kind: "ellipse";
  layer: string;
  center: Vec2;
  major_axis: Vec2;
  ratio: number;
  start_angle?: number;
  end_angle?: number;
}

interface SplinePrim {
  kind: "spline";
  layer: string;
  control_points: Vec2[];
  closed?: boolean;
}

interface HatchPrim {
  kind: "hatch";
  layer: string;
  boundaries: Array<{ vertices: Vec2[]; closed: boolean }>;
}

interface TextPrim {
  kind: "text" | "m_text";
  layer: string;
  position: Vec2;
  height: number;
  content: string;
}

export type Primitive =
  | LinePrim
  | PolylinePrim
  | ArcPrim
  | CirclePrim
  | EllipsePrim
  | SplinePrim
  | HatchPrim
  | TextPrim;

interface DrawPrimitiveBody {
  entity_id: string;
  primitive: Primitive;
}

interface PaintOptions {
  /**
   * Layer names whose primitives should be drawn. When `undefined`,
   * every primitive is drawn regardless of its `layer` field.
   */
  visibleLayers?: Set<string>;
  /**
   * Optional explicit color per layer. When absent, falls back to
   * the canvas default stroke color.
   */
  layerColors?: Map<string, string>;
}

/**
 * Paint the given `primitives` into `ctx`, fitting them inside a
 * `canvasWidth × canvasHeight` viewport. Returns the number of
 * primitives actually drawn (skipping ones filtered out by the
 * layer visibility set, unsupported variants without fallbacks,
 * and primitives that fail to provide a finite bounding box).
 */
export function paintPrimitives(
  ctx: CanvasRenderingContext2D,
  primitives: Primitive[],
  canvasWidth: number,
  canvasHeight: number,
  opts: PaintOptions = {},
): number {
  if (primitives.length === 0) return 0;

  // Step 1 — bounding box for the world fit. Skip primitives that
  // don't contribute a finite box (e.g. an empty polyline). We
  // include the visibility filter here so an empty filtered set
  // produces an empty bounding box and we early-return.
  const visible = primitives.filter((p) =>
    opts.visibleLayers ? opts.visibleLayers.has(p.layer) : true,
  );
  if (visible.length === 0) return 0;
  const bbox = computeBbox(visible);
  if (!bbox) return 0;

  const margin = 24;
  const dx = bbox.maxX - bbox.minX || 1;
  const dy = bbox.maxY - bbox.minY || 1;
  const scaleX = (canvasWidth - margin * 2) / dx;
  const scaleY = (canvasHeight - margin * 2) / dy;
  const scale = Math.min(scaleX, scaleY);
  const cx = (bbox.minX + bbox.maxX) / 2;
  const cy = (bbox.minY + bbox.maxY) / 2;
  const tx = canvasWidth / 2 - cx * scale;
  // Negate Y to flip CAD Y-up into canvas Y-down.
  const ty = canvasHeight / 2 + cy * scale;

  const worldToScreen = (p: Vec2): Vec2 => [
    p[0] * scale + tx,
    -p[1] * scale + ty,
  ];

  let drawn = 0;
  for (const prim of visible) {
    const color =
      opts.layerColors?.get(prim.layer) ?? "#d0d6e0";
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    ctx.lineWidth = 1;
    if (paintOne(ctx, prim, worldToScreen, scale)) drawn += 1;
  }
  return drawn;
}

/**
 * Extract the `DrawPrimitive` body from each `EntityRecord`. Rows
 * with `kind !== "primitive"` are skipped (DXF imports only emit
 * primitive rows, but the project graph can also hold design /
 * BIM rows in the same project for federated workflows). Rows
 * whose `body.primitive` is missing or malformed are skipped
 * silently — the painter degrades to "draw what we can read"
 * rather than failing the whole canvas paint on one bad row.
 */
export function extractPrimitives(rows: EntityRecord[]): Primitive[] {
  const out: Primitive[] = [];
  for (const row of rows) {
    if (row.kind !== "primitive") continue;
    const body = row.body as unknown as DrawPrimitiveBody;
    const prim = body?.primitive;
    if (prim && typeof prim === "object" && "kind" in prim) {
      out.push(prim);
    }
  }
  return out;
}

interface Bbox {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

function computeBbox(primitives: Primitive[]): Bbox | null {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  const expand = (x: number, y: number) => {
    if (Number.isFinite(x) && Number.isFinite(y)) {
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
    }
  };
  for (const p of primitives) {
    switch (p.kind) {
      case "line":
        expand(p.start[0], p.start[1]);
        expand(p.end[0], p.end[1]);
        break;
      case "polyline":
        for (const v of p.vertices) expand(v.at[0], v.at[1]);
        break;
      case "arc":
      case "circle":
        expand(p.center[0] - p.radius, p.center[1] - p.radius);
        expand(p.center[0] + p.radius, p.center[1] + p.radius);
        break;
      case "ellipse": {
        const a = Math.hypot(p.major_axis[0], p.major_axis[1]);
        const b = a * p.ratio;
        expand(p.center[0] - a, p.center[1] - b);
        expand(p.center[0] + a, p.center[1] + b);
        break;
      }
      case "spline":
        for (const c of p.control_points) expand(c[0], c[1]);
        break;
      case "hatch":
        for (const bnd of p.boundaries) {
          for (const v of bnd.vertices) expand(v[0], v[1]);
        }
        break;
      case "text":
      case "m_text":
        expand(p.position[0], p.position[1]);
        expand(p.position[0] + p.height * 4, p.position[1] + p.height);
        break;
    }
  }
  if (!Number.isFinite(minX) || !Number.isFinite(maxX)) return null;
  return { minX, minY, maxX, maxY };
}

function paintOne(
  ctx: CanvasRenderingContext2D,
  p: Primitive,
  worldToScreen: (v: Vec2) => Vec2,
  scale: number,
): boolean {
  switch (p.kind) {
    case "line": {
      const a = worldToScreen(p.start);
      const b = worldToScreen(p.end);
      ctx.beginPath();
      ctx.moveTo(a[0], a[1]);
      ctx.lineTo(b[0], b[1]);
      ctx.stroke();
      return true;
    }
    case "polyline": {
      if (p.vertices.length === 0) return false;
      ctx.beginPath();
      const first = worldToScreen(p.vertices[0].at);
      ctx.moveTo(first[0], first[1]);
      for (let i = 1; i < p.vertices.length; i++) {
        const v = worldToScreen(p.vertices[i].at);
        ctx.lineTo(v[0], v[1]);
      }
      if (p.closed) ctx.closePath();
      ctx.stroke();
      return true;
    }
    case "arc": {
      const c = worldToScreen(p.center);
      const r = p.radius * scale;
      // Canvas arc: 0 = +X, sweeps clockwise (because Y is flipped).
      // Our world arcs sweep CCW from `start_angle` to `end_angle`
      // in degrees. After Y flip we need to *negate* the angle and
      // pass `anticlockwise=true` to keep the geometry consistent.
      const a0 = -(p.start_angle * Math.PI) / 180;
      const a1 = -(p.end_angle * Math.PI) / 180;
      ctx.beginPath();
      ctx.arc(c[0], c[1], r, a0, a1, true);
      ctx.stroke();
      return true;
    }
    case "circle": {
      const c = worldToScreen(p.center);
      const r = p.radius * scale;
      ctx.beginPath();
      ctx.arc(c[0], c[1], r, 0, Math.PI * 2);
      ctx.stroke();
      return true;
    }
    case "ellipse": {
      // Approximate with a polyline so we don't depend on the
      // Canvas ellipse API (which has axis-rotation semantics
      // that differ from DXF's major-axis vector).
      const segments = 64;
      const a = Math.hypot(p.major_axis[0], p.major_axis[1]);
      const b = a * p.ratio;
      const phi = Math.atan2(p.major_axis[1], p.major_axis[0]);
      const sa = (p.start_angle ?? 0) * (Math.PI / 180);
      const ea = (p.end_angle ?? 360) * (Math.PI / 180);
      ctx.beginPath();
      for (let i = 0; i <= segments; i++) {
        const t = sa + ((ea - sa) * i) / segments;
        const xLocal = a * Math.cos(t);
        const yLocal = b * Math.sin(t);
        const world: Vec2 = [
          p.center[0] + Math.cos(phi) * xLocal - Math.sin(phi) * yLocal,
          p.center[1] + Math.sin(phi) * xLocal + Math.cos(phi) * yLocal,
        ];
        const s = worldToScreen(world);
        if (i === 0) ctx.moveTo(s[0], s[1]);
        else ctx.lineTo(s[0], s[1]);
      }
      ctx.stroke();
      return true;
    }
    case "spline": {
      // Linear interpolation of control points as a polyline
      // fallback — proper Bézier / NURBS evaluation is Group D
      // territory (the path tracer's curve crate isn't wired into
      // the renderer yet). The line still gives the user spatial
      // continuity for the imported geometry.
      if (p.control_points.length === 0) return false;
      ctx.beginPath();
      const first = worldToScreen(p.control_points[0]);
      ctx.moveTo(first[0], first[1]);
      for (let i = 1; i < p.control_points.length; i++) {
        const v = worldToScreen(p.control_points[i]);
        ctx.lineTo(v[0], v[1]);
      }
      if (p.closed) ctx.closePath();
      ctx.stroke();
      return true;
    }
    case "hatch": {
      for (const bnd of p.boundaries) {
        if (bnd.vertices.length === 0) continue;
        ctx.beginPath();
        const first = worldToScreen(bnd.vertices[0]);
        ctx.moveTo(first[0], first[1]);
        for (let i = 1; i < bnd.vertices.length; i++) {
          const v = worldToScreen(bnd.vertices[i]);
          ctx.lineTo(v[0], v[1]);
        }
        if (bnd.closed) ctx.closePath();
        ctx.stroke();
      }
      return true;
    }
    case "text":
    case "m_text": {
      const pos = worldToScreen(p.position);
      ctx.font = `${Math.max(8, p.height * scale)}px sans-serif`;
      ctx.fillText(p.content, pos[0], pos[1]);
      return true;
    }
  }
}
