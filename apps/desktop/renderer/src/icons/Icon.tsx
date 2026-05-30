/**
 * Phase 17 Group B Task 7 — SVG icon system.
 *
 * The previous design replaced Unicode glyphs (`✎`, `▱`, `◫`, `◉`,
 * `⤓`, etc.) for mode rail and toolbar buttons. Those have three
 * structural problems that this module replaces all at once:
 *
 *  1. Glyph availability varies by font — `◫` and `▱` render as
 *     "tofu" boxes on Windows machines without the right font
 *     installed, and even when they render, the metrics differ
 *     between Inter, Segoe UI Symbol, and system fallbacks.
 *  2. Glyphs aren't sizeable independently of font-size — the
 *     mode rail wants 22px icons next to 11px text labels, which
 *     forces awkward ratios via `font-size`.
 *  3. Glyphs can't follow CSS colour transitions on hover/active
 *     reliably (some glyphs inherit `color`, some don't, depending
 *     on font shaping rules).
 *
 * Each icon is a 24×24 monochrome SVG path inheriting `currentColor`
 * (so `:hover`, `:focus-visible`, `.is-active`, `data-theme="dark"`
 * etc. all just work). Render via `<Icon name="design" size={22} />`.
 *
 * The icon set is closed: adding a new icon requires editing
 * `ICONS` here. This is intentional so a `data-testid="icon-foo"`
 * grep across the renderer enumerates every icon use site, and so
 * the type system catches typos at compile-time (`name` is the
 * `keyof typeof ICONS` union, not a free-form string).
 */

import type { CSSProperties } from "react";

/**
 * Every icon's SVG path data. Each path is authored to fit cleanly
 * inside a `0 0 24 24` viewBox at stroke-width=2, fill="none",
 * stroke-linecap="round", stroke-linejoin="round" — the line-icon
 * convention used by Lucide, Heroicons, Tabler, Phosphor (Light).
 * Choosing line icons over filled icons keeps the visual weight
 * consistent across mode rail (small, dense) and toolbars (larger,
 * action-y) without needing two separate icon weights.
 *
 * Coordinates are absolute (no `m`/`l` relative ops) so the paths
 * stay readable side-by-side and so future SVG-optimiser passes
 * (svgo) don't reflow them into something unreviewable. Curves use
 * cubic Béziers (`C`) over quadratic (`Q`) for the same reason.
 */
export const ICONS = {
  // ----- Mode rail -----
  home: "M3 11.5L12 4L21 11.5V20A1 1 0 0 1 20 21H14V14H10V21H4A1 1 0 0 1 3 20V11.5Z",
  design:
    "M4 20L8 16M8 16L14 10L16 8L18 6L20 4L18 2L16 4L14 6M8 16L4 20M14 6L18 10M14 6L8 12L10 14L16 8",
  draft:
    "M4 4H20V20H4V4ZM4 8H20M8 4V20M4 14H20M4 17H20M14 4V20",
  bim: "M12 3L21 8L12 13L3 8L12 3ZM3 13L12 18L21 13M3 18L12 23L21 18",
  render:
    "M5 7H8L10 5H14L16 7H19A1 1 0 0 1 20 8V18A1 1 0 0 1 19 19H5A1 1 0 0 1 4 18V8A1 1 0 0 1 5 7ZM12 9A4 4 0 1 0 12 17A4 4 0 0 0 12 9Z",
  deliver:
    "M21 8L21 19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3H16L21 8ZM12 12V18M12 18L9 15M12 18L15 15",
  settings:
    "M19.4 15A1.65 1.65 0 0 0 19.7 13C19.9 12.4 19.9 11.6 19.7 11A1.65 1.65 0 0 0 19.4 9L21 7.5L19 4L17 5A1.65 1.65 0 0 0 15 4.6A1.65 1.65 0 0 0 13 4.3L12.5 2H11.5L11 4.3A1.65 1.65 0 0 0 9 4.6A1.65 1.65 0 0 0 7 5L5 4L3 7.5L4.6 9A1.65 1.65 0 0 0 4.3 11C4.1 11.6 4.1 12.4 4.3 13A1.65 1.65 0 0 0 4.6 15L3 16.5L5 20L7 19A1.65 1.65 0 0 0 9 19.4A1.65 1.65 0 0 0 11 19.7L11.5 22H12.5L13 19.7A1.65 1.65 0 0 0 15 19.4A1.65 1.65 0 0 0 17 19L19 20L21 16.5L19.4 15ZM12 15A3 3 0 1 1 12 9A3 3 0 0 1 12 15Z",

  // ----- Design toolbar -----
  select:
    "M5 2L19 12L13 14L18 19L16 21L11 16L5 22V2Z",
  wall:
    "M3 4H21M3 4V20H21V4M7 4V20M11 4V20M15 4V20M19 4V20",
  floor:
    "M3 6L12 3L21 6L12 9L3 6ZM3 6V18L12 21M21 6V18L12 21M12 9V21",
  ceiling:
    "M3 6L12 9L21 6L12 3L3 6ZM3 6V12L12 15L21 12V6",
  door: "M6 22V4A2 2 0 0 1 8 2H16A2 2 0 0 1 18 4V22M6 22H4M6 22H18M18 22H20M14 13H15",
  window:
    "M4 4H20V20H4V4ZM12 4V20M4 12H20",
  furniture:
    "M4 12V19H6V17H18V19H20V12A4 4 0 0 0 16 8H8A4 4 0 0 0 4 12ZM6 13H18",
  material:
    "M12 2A10 10 0 1 0 22 12A10 10 0 0 0 12 2ZM12 2V12L20.66 7M12 12L3.34 7M12 12V22",
  lighting:
    "M9 18H15M10 21H14M12 3A6 6 0 0 0 8 13.5C8 15 9 16 9 17H15C15 16 16 15 16 13.5A6 6 0 0 0 12 3Z",
  camera:
    "M3 7A2 2 0 0 1 5 5H8L10 3H14L16 5H19A2 2 0 0 1 21 7V18A2 2 0 0 1 19 20H5A2 2 0 0 1 3 18V7ZM12 17A4.5 4.5 0 1 0 12 8A4.5 4.5 0 0 0 12 17Z",

  // ----- Draft toolbar (drawing) -----
  line: "M4 20L20 4",
  polyline: "M4 20L8 8L12 14L16 6L20 16",
  circle: "M12 3A9 9 0 1 0 12 21A9 9 0 0 0 12 3Z",
  arc: "M4 20A12 12 0 0 1 20 20",
  text: "M5 5H19M12 5V19M9 19H15",
  dim: "M4 12H20M4 12L8 8M4 12L8 16M20 12L16 8M20 12L16 16",
  hatch:
    "M4 4H20V20H4V4ZM4 8L8 4M4 12L12 4M4 16L16 4M4 20L20 4M8 20L20 8M12 20L20 12M16 20L20 16",

  // ----- Draft toolbar (editing) -----
  move: "M12 3V21M3 12H21M12 3L9 6M12 3L15 6M12 21L9 18M12 21L15 18M3 12L6 9M3 12L6 15M21 12L18 9M21 12L18 15",
  copy: "M9 8H19A1 1 0 0 1 20 9V19A1 1 0 0 1 19 20H9A1 1 0 0 1 8 19V9A1 1 0 0 1 9 8ZM5 4H15A1 1 0 0 1 16 5V8M5 4A1 1 0 0 0 4 5V15A1 1 0 0 0 5 16H8",
  rotate:
    "M21 12A9 9 0 1 1 12 3M21 12L18 9M21 12L18 15",
  trim: "M4 4L20 20M4 20L20 4M9 9L15 15",
  extend: "M4 12H14M14 12L10 8M14 12L10 16M16 4V20",
  offset:
    "M4 8H20V20H4V8ZM4 4H20",
  fillet: "M4 4V12A8 8 0 0 0 12 20H20",
  chamfer: "M4 4V12L12 20H20",
  mirror: "M12 3V21M5 8L9 12L5 16M19 8L15 12L19 16",
  scale:
    "M3 21V14M3 21H10M3 21L10 14M21 3V10M21 3H14M21 3L14 10",

  // ----- BIM toolbar -----
  importIfc:
    "M4 14V18A2 2 0 0 0 6 20H18A2 2 0 0 0 20 18V14M12 4V14M12 14L8 10M12 14L16 10",
  attachIfc:
    "M9 13L13 9A4 4 0 1 1 19 15L13 21A6 6 0 1 1 5 13L11 7",
  exportIfc:
    "M4 14V18A2 2 0 0 0 6 20H18A2 2 0 0 0 20 18V14M12 14V4M12 4L8 8M12 4L16 8",
  validate:
    "M9 12L11 14L15 10M12 3L4 7V12C4 16.5 7 19.5 12 21C17 19.5 20 16.5 20 12V7L12 3Z",
  classify:
    "M3 7H21M3 12H21M3 17H21M7 3V21M14 3V21",
  schedule:
    "M4 4H20V20H4V4ZM4 9H20M9 4V20M9 14H14",
  diff: "M9 4L9 14M9 14L5 10M9 14L13 10M15 20L15 10M15 10L11 14M15 10L19 14",
  boq: "M4 4H20V8H4V4ZM4 10H20V14H4V10ZM4 16H20V20H4V16ZM7 6V6M7 12V12M7 18V18",

  // ----- Deliver toolbar -----
  exportPack: "M21 12V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V12M16 6L12 2L8 6M12 2V15",
  tagRevision:
    "M12 4L20 4V12L11 21A2 2 0 0 1 8 21L3 16A2 2 0 0 1 3 13L12 4ZM16 8A0.5 0.5 0 1 1 16 9A0.5 0.5 0 0 1 16 8",
  compareRevisions:
    "M10 4V20M10 4L6 8M10 4L14 8M14 20L14 4M14 20L10 16M14 20L18 16",

  // ----- Generic -----
  close: "M5 5L19 19M5 19L19 5",
  check: "M5 12L10 17L19 8",
  chevronRight: "M9 6L15 12L9 18",
  chevronDown: "M6 9L12 15L18 9",
  chevronUp: "M6 15L12 9L18 15",
  ellipsis:
    "M7 12A1 1 0 1 1 7 12.001M12 12A1 1 0 1 1 12 12.001M17 12A1 1 0 1 1 17 12.001",
  search: "M11 4A7 7 0 1 0 11 18A7 7 0 0 0 11 4ZM21 21L16 16",
  copyCoords: "M9 2H17A1 1 0 0 1 18 3V11A1 1 0 0 1 17 12H9A1 1 0 0 1 8 11V3A1 1 0 0 1 9 2ZM4 7V21A1 1 0 0 0 5 22H15",
  snapshot:
    "M3 8A2 2 0 0 1 5 6H7L9 4H15L17 6H19A2 2 0 0 1 21 8V18A2 2 0 0 1 19 20H5A2 2 0 0 1 3 18V8ZM12 9V15M9 12H15",
  trash:
    "M4 7H20M9 7V5A2 2 0 0 1 11 3H13A2 2 0 0 1 15 5V7M6 7V20A1 1 0 0 0 7 21H17A1 1 0 0 0 18 20V7",
  lock: "M6 11V8A6 6 0 0 1 18 8V11M5 11H19A1 1 0 0 1 20 12V20A1 1 0 0 1 19 21H5A1 1 0 0 1 4 20V12A1 1 0 0 1 5 11Z",
  unlock: "M6 11V8A6 6 0 0 1 17 5M5 11H19A1 1 0 0 1 20 12V20A1 1 0 0 1 19 21H5A1 1 0 0 1 4 20V12A1 1 0 0 1 5 11Z",
  // 3-dot grip used by the panel resize handles (Task 14).
  grip: "M9 6A1 1 0 1 1 9 6.001M9 12A1 1 0 1 1 9 12.001M9 18A1 1 0 1 1 9 18.001M15 6A1 1 0 1 1 15 6.001M15 12A1 1 0 1 1 15 12.001M15 18A1 1 0 1 1 15 18.001",
} as const;

export type IconName = keyof typeof ICONS;

export interface IconProps {
  name: IconName;
  /**
   * Pixel size for the rendered `<svg>` (`width = height = size`).
   * Default 20 matches the toolbar button size used across the app.
   * The mode rail passes 22, the brand renders larger, etc.
   */
  size?: number;
  /**
   * Optional `aria-label` for screen readers. When omitted the icon
   * is rendered with `aria-hidden="true"` — the convention for icons
   * inside a button that already has a `title` / `aria-label`.
   */
  label?: string;
  /**
   * Optional `data-testid`. When omitted, `icon-${name}` is used so
   * snapshot/unit tests can grep deterministically.
   */
  "data-testid"?: string;
  className?: string;
  style?: CSSProperties;
  /**
   * Override the default `stroke-width`. Most icons look correct at
   * 2px on a 24-unit viewBox; the brand block / dense diagrams may
   * benefit from 1.5px for visual density.
   */
  strokeWidth?: number;
}

/**
 * Render a single line-style SVG icon. The component is a thin
 * presentational wrapper — no state, no effects, no portals — so it
 * tree-shakes cleanly and snapshot tests run in microseconds.
 *
 * The svg inherits `currentColor` for `stroke`, so wrapping
 * `<Icon />` in any element that controls `color` cascades through.
 * Buttons in the existing toolbars already do this via
 * `--aec-color-text-secondary` / `.is-active` etc.
 */
export function Icon({
  name,
  size = 20,
  label,
  className,
  style,
  strokeWidth = 2,
  ...rest
}: IconProps): JSX.Element {
  const path = ICONS[name];
  const testId = rest["data-testid"] ?? `icon-${name}`;
  const hidden = label === undefined;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={hidden ? "true" : undefined}
      aria-label={hidden ? undefined : label}
      role={hidden ? undefined : "img"}
      data-testid={testId}
      data-icon={name}
      className={className}
      style={style}
    >
      <path d={path} />
    </svg>
  );
}
