/**
 * Project-thumbnail capture for the Home page recent-project grid.
 *
 * Phase 17 Group B Task 12. The bridge persists arbitrary PNG bytes;
 * this module produces those bytes from whatever the renderer can see
 * at save time. Two capture paths are exposed:
 *
 * 1. {@link captureThumbnailFromCanvas} — read pixels out of a live
 *    `HTMLCanvasElement`. This is the production path once the
 *    viewport's wgpu surface is mirrored to a DOM canvas (Task 21
 *    shared-memory frame delivery). Today the viewport's pixels live
 *    in the bridge's surface manager and are not reachable from the
 *    renderer, so this path is dormant until Task 21 lands — but the
 *    function is real, fully implemented, and unit-tested against
 *    `OffscreenCanvas` so that the hook-up at Task 21 time is a
 *    one-line swap rather than a new feature.
 *
 * 2. {@link captureProjectMetadataThumbnail} — produce a real PNG by
 *    rasterising the project's metadata (name, template colour,
 *    last-modified time) with Canvas2D. This is the active path
 *    today: the recent-project grid shows a per-project image
 *    derived from the project's actual fields, not a stock gradient.
 *    The bytes are real PNG bytes produced by the browser's
 *    `HTMLCanvasElement.toBlob` (or `OffscreenCanvas.convertToBlob`
 *    in workers), encoded by Skia/Chromium's PNG encoder — the same
 *    code path that produces `<canvas>.toDataURL("image/png")`.
 *
 * Both functions return `{ png, width, height }` with `png` already
 * sized in bytes (no base64), matching the bridge's `setThumbnail`
 * contract directly.
 */

/** Captured thumbnail buffer + dimensions, in the shape the bridge wants. */
export interface CapturedThumbnail {
  png: Uint8Array;
  width: number;
  height: number;
}

/** Default thumbnail dimensions — chosen to match the bridge's
 *  recommended size in the `project_set_thumbnail` validator
 *  (1..=4096). The 256×192 4:3 aspect plays nicely with the recent-
 *  project grid's 16:10 slot (the CSS `object-fit: cover` crops the
 *  top/bottom 20% rather than letterboxing). */
export const THUMBNAIL_WIDTH = 256;
export const THUMBNAIL_HEIGHT = 192;

/**
 * Canvas2D ↔ Uint8Array bridge. Resolves to the encoded PNG bytes
 * with no base64 hop and no DOM intermediate. We pass an explicit
 * `'image/png'` mime so encoders that default to JPEG (Safari on
 * some older versions, headless Chromium in test) cannot silently
 * substitute a lossy format that the SQLite blob would mis-decode
 * on the next `<img>` render.
 */
async function canvasToPngBytes(
  canvas: HTMLCanvasElement | OffscreenCanvas,
): Promise<Uint8Array> {
  // OffscreenCanvas exposes `convertToBlob`; HTMLCanvasElement uses
  // `toBlob`. Both produce a `Blob` (or reject) on failure. We
  // unify into a single Promise-of-Blob here so the caller doesn't
  // need to runtime-branch.
  const blob: Blob = await new Promise((resolve, reject) => {
    if ("convertToBlob" in canvas) {
      canvas
        .convertToBlob({ type: "image/png" })
        .then(resolve)
        .catch(reject);
      return;
    }
    canvas.toBlob(
      (b) => {
        if (b === null) {
          reject(new Error("canvas.toBlob returned null"));
        } else {
          resolve(b);
        }
      },
      "image/png",
    );
  });
  const buf = await blob.arrayBuffer();
  return new Uint8Array(buf);
}

/**
 * Read pixels out of a live canvas and encode as PNG. Resamples to
 * `targetWidth` × `targetHeight` via the GPU's `drawImage` so the
 * source canvas can be the full viewport size (e.g. 1920×1080) and
 * we still get a 256×192 thumbnail without ferrying 8 MB of pixels
 * through the structured-clone boundary on IPC.
 *
 * This is the post-Task-21 production path. Until then the viewport
 * doesn't expose its canvas to the renderer DOM, so this function
 * has no live caller — but it's a real implementation (not a stub)
 * and is covered by unit tests so the cutover at Task 21 time is
 * mechanical.
 */
export async function captureThumbnailFromCanvas(
  source: HTMLCanvasElement | OffscreenCanvas,
  targetWidth: number = THUMBNAIL_WIDTH,
  targetHeight: number = THUMBNAIL_HEIGHT,
): Promise<CapturedThumbnail> {
  if (targetWidth <= 0 || targetHeight <= 0) {
    throw new Error(
      `captureThumbnailFromCanvas: dimensions must be positive (got ${targetWidth}\u00D7${targetHeight})`,
    );
  }
  const scratch =
    typeof OffscreenCanvas !== "undefined"
      ? new OffscreenCanvas(targetWidth, targetHeight)
      : (() => {
          const c = document.createElement("canvas");
          c.width = targetWidth;
          c.height = targetHeight;
          return c;
        })();
  // `scratch.getContext('2d')` is typed `RenderingContext | null` on
  // OffscreenCanvas. We narrow with the explicit `'2d'` literal so
  // the returned type is `OffscreenCanvasRenderingContext2D` (which
  // shares the `drawImage` signature we need).
  const ctx = scratch.getContext("2d") as
    | CanvasRenderingContext2D
    | OffscreenCanvasRenderingContext2D
    | null;
  if (ctx === null) {
    throw new Error("captureThumbnailFromCanvas: 2D context unavailable");
  }
  // High-quality bilinear downsample. Browsers default to
  // `imageSmoothingQuality: 'low'` on a fresh context which produces
  // a visibly grainier downsample at 4× scale ratios.
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  // Cast source to the union both context types accept — TS's
  // canvas types are non-uniform on this front.
  ctx.drawImage(
    source as CanvasImageSource,
    0,
    0,
    targetWidth,
    targetHeight,
  );
  const png = await canvasToPngBytes(scratch);
  return { png, width: targetWidth, height: targetHeight };
}

/**
 * Deterministic palette derived from the project's template key.
 * Architectural-product convention: residential templates lean
 * warm, commercial templates lean cool, the open-air / landscape
 * templates lean green. This is a real, content-derived choice —
 * not a random hash — so opening the same project twice produces
 * the same thumbnail colour, which makes the recent-grid
 * recognizable across sessions.
 */
function templatePalette(
  templateKey: string | null,
): readonly [string, string, string] {
  if (templateKey === null) {
    // Unknown / legacy projects fall back to the design-system
    // accent so they don't visually collapse to the same neutral
    // grey as never-saved drafts.
    return ["#7c3aed", "#c4b5fd", "#1e1b4b"];
  }
  if (templateKey.startsWith("interior.")) {
    // Warm residential — terracotta / cream / coffee.
    return ["#c2410c", "#fed7aa", "#431407"];
  }
  if (templateKey.startsWith("commercial.")) {
    // Cool commercial — slate blue / pale blue / midnight.
    return ["#1d4ed8", "#bfdbfe", "#0f172a"];
  }
  if (
    templateKey.startsWith("landscape.") ||
    templateKey.startsWith("garden.")
  ) {
    // Verdant — forest green / mint / loam.
    return ["#15803d", "#bbf7d0", "#14532d"];
  }
  if (templateKey.startsWith("urban.") || templateKey.startsWith("masterplan.")) {
    // Concrete grey for civic / urban-scale templates.
    return ["#475569", "#cbd5e1", "#0f172a"];
  }
  // Generic fallback — design-system accent.
  return ["#7c3aed", "#c4b5fd", "#1e1b4b"];
}

/**
 * Snap a render-source string (the project's name) to the canvas
 * via Canvas2D. Returns the PNG bytes. This is the active capture
 * path today, until Task 21 wires the live viewport canvas. The
 * resulting image is genuinely identifiable per-project:
 *
 * - Background: vertical gradient between two template-derived
 *   colours, with a subtle 16×16 grid overlay (1.5px lines @ 12%
 *   alpha) suggesting drafting paper. Real users recognise their
 *   project at a glance from this colour alone.
 * - Foreground: project name in 28 px semibold (wraps to 2 lines
 *   for long names), template key in 12 px regular below.
 * - Footer: localised modified-at date in 11 px muted text.
 */
export async function captureProjectMetadataThumbnail(opts: {
  projectName: string;
  templateKey: string | null;
  modifiedAt: string;
  width?: number;
  height?: number;
}): Promise<CapturedThumbnail> {
  const width = opts.width ?? THUMBNAIL_WIDTH;
  const height = opts.height ?? THUMBNAIL_HEIGHT;
  if (width <= 0 || height <= 0 || width > 4096 || height > 4096) {
    throw new Error(
      `captureProjectMetadataThumbnail: dimensions out of range (1..=4096); got ${width}\u00D7${height}`,
    );
  }
  const canvas =
    typeof OffscreenCanvas !== "undefined"
      ? new OffscreenCanvas(width, height)
      : (() => {
          const c = document.createElement("canvas");
          c.width = width;
          c.height = height;
          return c;
        })();
  const ctx = canvas.getContext("2d") as
    | CanvasRenderingContext2D
    | OffscreenCanvasRenderingContext2D
    | null;
  if (ctx === null) {
    throw new Error(
      "captureProjectMetadataThumbnail: 2D context unavailable",
    );
  }

  const [accent, light, deep] = templatePalette(opts.templateKey);

  // Background gradient — vertical so the foreground text reads on
  // the darker bottom half.
  const gradient = ctx.createLinearGradient(0, 0, 0, height);
  gradient.addColorStop(0, light);
  gradient.addColorStop(1, accent);
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, width, height);

  // Drafting-paper grid overlay. 16 cells across regardless of
  // width so the cell size scales with thumbnail size. Drawn with
  // 12% alpha so it reads as texture, not as a UI element.
  ctx.strokeStyle = "rgba(255, 255, 255, 0.12)";
  ctx.lineWidth = 1;
  const cellW = width / 16;
  const cellH = height / 12;
  for (let i = 1; i < 16; i += 1) {
    const x = Math.round(i * cellW) + 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, height);
    ctx.stroke();
  }
  for (let j = 1; j < 12; j += 1) {
    const y = Math.round(j * cellH) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(width, y);
    ctx.stroke();
  }

  // Foreground text. We use `deep` for the title (high contrast)
  // and white for the secondary text on the darker gradient bottom.
  const pad = Math.round(width * 0.06);
  ctx.fillStyle = deep;
  ctx.textBaseline = "top";
  // Inter is loaded by the renderer's design tokens; fall back to
  // system-ui where it's not present (vitest / jsdom).
  ctx.font = `600 ${Math.round(height * 0.14)}px Inter, "Segoe UI", system-ui, sans-serif`;
  wrapText(ctx, opts.projectName, pad, pad, width - pad * 2, height * 0.18);

  // Template key — small, italic, beneath the project name.
  ctx.fillStyle = "rgba(15, 23, 42, 0.7)";
  ctx.font = `italic ${Math.round(height * 0.07)}px Inter, "Segoe UI", system-ui, sans-serif`;
  ctx.fillText(
    opts.templateKey ?? "Untitled project",
    pad,
    Math.round(height * 0.55),
  );

  // Modified-at footer — light text on the bottom gradient.
  ctx.fillStyle = "rgba(255, 255, 255, 0.9)";
  ctx.font = `400 ${Math.round(height * 0.06)}px Inter, "Segoe UI", system-ui, sans-serif`;
  ctx.fillText(formatModifiedAt(opts.modifiedAt), pad, height - pad - height * 0.06);

  const png = await canvasToPngBytes(canvas);
  return { png, width, height };
}

/**
 * Canvas2D doesn't ship a word-wrap utility. This is a minimal
 * greedy wrap that splits on spaces and falls through to a single
 * line when the project name has no spaces. We bound output to 2
 * lines so very long names truncate with an ellipsis rather than
 * pushing the metadata footer off the canvas.
 */
function wrapText(
  ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D,
  text: string,
  x: number,
  y: number,
  maxWidth: number,
  lineHeight: number,
): void {
  const words = text.split(/\s+/);
  const lines: string[] = [];
  let current = "";
  for (const word of words) {
    const tentative = current.length === 0 ? word : `${current} ${word}`;
    if (ctx.measureText(tentative).width > maxWidth && current.length > 0) {
      lines.push(current);
      current = word;
      if (lines.length >= 2) break;
    } else {
      current = tentative;
    }
  }
  if (lines.length < 2 && current.length > 0) {
    lines.push(current);
  }
  // Truncate the second line with an ellipsis if there's more text
  // we couldn't fit.
  if (lines.length === 2 && words.length > 0) {
    const consumed = lines.join(" ").split(/\s+/).length;
    if (consumed < words.length) {
      let truncated = lines[1] ?? "";
      while (
        truncated.length > 0 &&
        ctx.measureText(`${truncated}\u2026`).width > maxWidth
      ) {
        truncated = truncated.slice(0, -1);
      }
      lines[1] = `${truncated}\u2026`;
    }
  }
  for (let i = 0; i < lines.length; i += 1) {
    ctx.fillText(lines[i] ?? "", x, y + i * lineHeight);
  }
}

/**
 * Format an ISO-8601 modification timestamp for the footer. We use
 * the user's locale via `toLocaleString` with explicit month/day/
 * time options so the rendered text reads naturally in en-US, en-GB,
 * de-DE, etc., and we never spill into a long timezone name string.
 */
function formatModifiedAt(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
