import { afterEach, describe, expect, it, vi } from "vitest";

import {
  captureProjectMetadataThumbnail,
  captureThumbnailFromCanvas,
  THUMBNAIL_HEIGHT,
  THUMBNAIL_WIDTH,
} from "../lib/thumbnail";

/**
 * Phase 17 Group B Task 12 — thumbnail capture coverage. jsdom 24
 * does not ship Canvas2D — `<canvas>.getContext('2d')` returns
 * `null` and `toBlob` does not fire its callback. To exercise the
 * real production code paths (palette selection, gradient
 * composition, word-wrap, footer rendering, PNG-bytes return shape)
 * we install a faithful fake on `HTMLCanvasElement.prototype` that
 * records the calls our code makes and returns a synthetic PNG
 * blob. The fake is restored after each test so other suites that
 * tolerate the jsdom default keep working.
 */

interface RecordedGradient {
  stops: Array<{ offset: number; color: string }>;
}

interface RecordedCtx {
  fillStyles: string[];
  fillRects: Array<{ x: number; y: number; w: number; h: number }>;
  fillTexts: Array<{ text: string; x: number; y: number }>;
  strokes: number;
  lastFont: string | null;
  gradients: RecordedGradient[];
}

// Minimal valid 1×1 PNG (signature + IHDR with bogus CRC ok for our
// purposes — the renderer code never re-decodes the bytes; the test
// just asserts that the bytes come back unchanged from the encoder).
const SYNTHETIC_PNG = new Uint8Array([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
  0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
  0x89,
]);

function installCanvasFake(): { recorded: RecordedCtx; restore: () => void } {
  const recorded: RecordedCtx = {
    fillStyles: [],
    fillRects: [],
    fillTexts: [],
    strokes: 0,
    lastFont: null,
    gradients: [],
  };

  const proto = HTMLCanvasElement.prototype as unknown as {
    getContext: (id: string) => unknown;
    toBlob: (
      cb: (b: Blob | null) => void,
      type?: string,
    ) => void;
  };
  const originalGetContext = proto.getContext;
  const originalToBlob = proto.toBlob;

  proto.getContext = function fakeGetContext(id: string): unknown {
    if (id !== "2d") return null;
    return {
      createLinearGradient(_x0: number, _y0: number, _x1: number, _y1: number) {
        const g: RecordedGradient = { stops: [] };
        recorded.gradients.push(g);
        return {
          addColorStop(offset: number, color: string) {
            g.stops.push({ offset, color });
          },
        };
      },
      set fillStyle(s: string) {
        recorded.fillStyles.push(s);
      },
      get fillStyle() {
        return recorded.fillStyles[recorded.fillStyles.length - 1] ?? "";
      },
      set strokeStyle(_s: string) {},
      set lineWidth(_n: number) {},
      set font(s: string) {
        recorded.lastFont = s;
      },
      set textBaseline(_s: string) {},
      set imageSmoothingEnabled(_b: boolean) {},
      set imageSmoothingQuality(_q: string) {},
      fillRect(x: number, y: number, w: number, h: number) {
        recorded.fillRects.push({ x, y, w, h });
      },
      fillText(text: string, x: number, y: number) {
        recorded.fillTexts.push({ text, x, y });
      },
      beginPath() {},
      moveTo() {},
      lineTo() {},
      stroke() {
        recorded.strokes += 1;
      },
      // Wrap-helper measure: deterministic by length × 7 px/char so
      // tests can predict whether a string fits inside `maxWidth`.
      measureText(text: string) {
        return { width: text.length * 7 };
      },
      drawImage() {},
    };
  };

  proto.toBlob = function fakeToBlob(
    cb: (b: Blob | null) => void,
    type?: string,
  ) {
    expect(type).toBe("image/png");
    // jsdom 24's `Blob` constructor exists but the instance is
    // missing `arrayBuffer()` (it was added to the spec after the
    // jsdom v24 ship date). We assemble a minimal Blob-compatible
    // value that the function under test can consume.
    const blob = {
      type: "image/png",
      size: SYNTHETIC_PNG.byteLength,
      arrayBuffer: () =>
        Promise.resolve(
          SYNTHETIC_PNG.buffer.slice(
            SYNTHETIC_PNG.byteOffset,
            SYNTHETIC_PNG.byteOffset + SYNTHETIC_PNG.byteLength,
          ),
        ),
      slice: () => blob,
      stream: () => {
        throw new Error("not implemented in test");
      },
      text: () => Promise.resolve(""),
    } as unknown as Blob;
    setTimeout(() => cb(blob), 0);
  };

  return {
    recorded,
    restore: () => {
      proto.getContext = originalGetContext;
      proto.toBlob = originalToBlob;
    },
  };
}

describe("captureProjectMetadataThumbnail", () => {
  let restore: (() => void) | null = null;
  afterEach(() => {
    restore?.();
    restore = null;
  });

  it("uses the warm residential palette for interior.* templates", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    const { png, width, height } = await captureProjectMetadataThumbnail({
      projectName: "Helsinki apartment",
      templateKey: "interior.apartment",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(width).toBe(THUMBNAIL_WIDTH);
    expect(height).toBe(THUMBNAIL_HEIGHT);
    expect(png).toBeInstanceOf(Uint8Array);
    expect(png.length).toBe(SYNTHETIC_PNG.length);
    // PNG signature byte preserved.
    expect(png[0]).toBe(0x89);
    expect(f.recorded.gradients).toHaveLength(1);
    // Warm residential = terracotta accent + cream highlight.
    const stops = f.recorded.gradients[0]!.stops;
    expect(stops.map((s) => s.color)).toEqual(["#fed7aa", "#c2410c"]);
    // The project name shows up in fillText.
    expect(f.recorded.fillTexts.some((t) => t.text === "Helsinki apartment")).toBe(
      true,
    );
    // The template key shows up in fillText.
    expect(
      f.recorded.fillTexts.some((t) => t.text === "interior.apartment"),
    ).toBe(true);
  });

  it("uses the cool commercial palette for commercial.* templates", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    await captureProjectMetadataThumbnail({
      projectName: "Helsinki office",
      templateKey: "commercial.office",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(f.recorded.gradients[0]!.stops.map((s) => s.color)).toEqual([
      "#bfdbfe",
      "#1d4ed8",
    ]);
  });

  it("uses the verdant palette for landscape.* and garden.* templates", async () => {
    const f1 = installCanvasFake();
    restore = f1.restore;
    await captureProjectMetadataThumbnail({
      projectName: "Park",
      templateKey: "landscape.park",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(f1.recorded.gradients[0]!.stops.map((s) => s.color)).toEqual([
      "#bbf7d0",
      "#15803d",
    ]);

    restore!();
    const f2 = installCanvasFake();
    restore = f2.restore;
    await captureProjectMetadataThumbnail({
      projectName: "Front garden",
      templateKey: "garden.front",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(f2.recorded.gradients[0]!.stops.map((s) => s.color)).toEqual([
      "#bbf7d0",
      "#15803d",
    ]);
  });

  it("falls back to the accent palette for unknown / null templates", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    await captureProjectMetadataThumbnail({
      projectName: "Untitled",
      templateKey: null,
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(f.recorded.gradients[0]!.stops.map((s) => s.color)).toEqual([
      "#c4b5fd",
      "#7c3aed",
    ]);
    expect(
      f.recorded.fillTexts.some((t) => t.text === "Untitled project"),
    ).toBe(true);
  });

  it("falls back to the accent palette for unrecognised prefixes", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    await captureProjectMetadataThumbnail({
      projectName: "Mystery",
      templateKey: "spaceship.uss-enterprise",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    expect(f.recorded.gradients[0]!.stops.map((s) => s.color)).toEqual([
      "#c4b5fd",
      "#7c3aed",
    ]);
  });

  it("word-wraps long names to two lines and ellipsises overflow", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    // With the fake `measureText` of 7 px/char and the default
    // 256-wide thumbnail with 6% padding (≈226 px usable), each line
    // should hold ~32 characters before wrapping.
    await captureProjectMetadataThumbnail({
      projectName:
        "an extremely long project title that absolutely cannot fit on a single thumbnail line at all",
      templateKey: "interior.apartment",
      modifiedAt: "2025-05-30T08:00:00.000Z",
    });
    const titleLines = f.recorded.fillTexts.filter(
      (t) =>
        t.text !== "interior.apartment" &&
        !t.text.includes(":") /* footer time */ &&
        t.text.length > 0,
    );
    expect(titleLines.length).toBeGreaterThanOrEqual(1);
    expect(titleLines.length).toBeLessThanOrEqual(2);
    // The last title line must end in an ellipsis if there was
    // overflow (more than 2 wrapped lines' worth of words).
    const lastLine = titleLines[titleLines.length - 1]!.text;
    expect(lastLine.endsWith("\u2026")).toBe(true);
  });

  it("rejects 0 or > 4096 dimensions before touching the DOM", async () => {
    const f = installCanvasFake();
    restore = f.restore;
    await expect(
      captureProjectMetadataThumbnail({
        projectName: "x",
        templateKey: null,
        modifiedAt: "2025-05-30T08:00:00.000Z",
        width: 0,
      }),
    ).rejects.toThrow(/dimensions out of range/);
    await expect(
      captureProjectMetadataThumbnail({
        projectName: "x",
        templateKey: null,
        modifiedAt: "2025-05-30T08:00:00.000Z",
        height: 5000,
      }),
    ).rejects.toThrow(/dimensions out of range/);
    // The dimension check fires before any context is acquired.
    expect(f.recorded.gradients).toHaveLength(0);
  });

  it("throws a clear error when the 2D context is unavailable", async () => {
    // Don't install the fake — let jsdom's null context surface.
    const proto = HTMLCanvasElement.prototype as unknown as {
      getContext: (id: string) => unknown;
    };
    const original = proto.getContext;
    proto.getContext = function () {
      return null;
    };
    try {
      // Skip the OffscreenCanvas fast-path so the test exercises
      // the document.createElement branch.
      const originalOffscreen =
        (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas;
      (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas =
        undefined;
      try {
        await expect(
          captureProjectMetadataThumbnail({
            projectName: "x",
            templateKey: null,
            modifiedAt: "2025-05-30T08:00:00.000Z",
          }),
        ).rejects.toThrow(/2D context unavailable/);
      } finally {
        (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas =
          originalOffscreen;
      }
    } finally {
      proto.getContext = original;
    }
  });
});

describe("captureThumbnailFromCanvas", () => {
  it("validates target dimensions before drawing", async () => {
    // Use a real `HTMLCanvasElement` (its `getContext` may return
    // null in jsdom, but we never reach that path because the
    // dimension check fires first).
    const source = document.createElement("canvas");
    await expect(
      captureThumbnailFromCanvas(source, 0, 192),
    ).rejects.toThrow(/dimensions must be positive/);
    await expect(
      captureThumbnailFromCanvas(source, 256, -1),
    ).rejects.toThrow(/dimensions must be positive/);
  });

  it("downsamples the source canvas to the target dimensions", async () => {
    const f = installCanvasFake();
    try {
      // Skip the OffscreenCanvas branch so the test exercises the
      // `document.createElement('canvas')` fallback (the only path
      // jsdom can host).
      const originalOffscreen =
        (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas;
      (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas =
        undefined;
      try {
        const source = document.createElement("canvas");
        source.width = 1920;
        source.height = 1080;
        const out = await captureThumbnailFromCanvas(source, 128, 96);
        expect(out.width).toBe(128);
        expect(out.height).toBe(96);
        expect(out.png).toBeInstanceOf(Uint8Array);
        expect(out.png[0]).toBe(0x89);
      } finally {
        (globalThis as unknown as { OffscreenCanvas?: unknown }).OffscreenCanvas =
          originalOffscreen;
      }
    } finally {
      f.restore();
    }
  });
});

// Avoid the unused-import lint when refactoring.
void vi;
