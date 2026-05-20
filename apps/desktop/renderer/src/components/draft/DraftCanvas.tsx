import { useEffect, useRef, useState } from "react";
import type { DraftTool } from "./DraftToolbar";

interface Props {
  activeTool: DraftTool;
  /** Optional callback when the user clicks in model space. */
  onPick?: (worldX: number, worldY: number) => void;
}

/**
 * Lightweight 2D canvas backed by a 2D context. The native CAD canvas
 * (wgpu) is hosted in the Rust `aec_viewport` crate; in unit/component
 * tests we can't create a wgpu surface, so we paint a minimal grid +
 * crosshair to verify the picking math is correct. The production
 * Electron build replaces the inner div with the wgpu surface mount.
 */
export function DraftCanvas({ activeTool, onPick }: Props) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null);
  const [size, setSize] = useState<{ w: number; h: number }>({ w: 600, h: 400 });

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    canvas.width = size.w;
    canvas.height = size.h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    // Grid.
    ctx.fillStyle = "#1a1a1a";
    ctx.fillRect(0, 0, size.w, size.h);
    ctx.strokeStyle = "#2c2c2c";
    ctx.lineWidth = 1;
    for (let x = 0; x <= size.w; x += 20) {
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, size.h);
      ctx.stroke();
    }
    for (let y = 0; y <= size.h; y += 20) {
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(size.w, y);
      ctx.stroke();
    }
    // Crosshair.
    if (cursor) {
      ctx.strokeStyle = "#ffe066";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, cursor.y);
      ctx.lineTo(size.w, cursor.y);
      ctx.moveTo(cursor.x, 0);
      ctx.lineTo(cursor.x, size.h);
      ctx.stroke();
    }
  }, [size, cursor]);

  return (
    <section
      className="draft-canvas"
      data-testid="draft-canvas"
      data-active-tool={activeTool}
    >
      <canvas
        ref={ref}
        data-testid="draft-canvas-surface"
        onMouseMove={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          setCursor({ x: e.clientX - rect.left, y: e.clientY - rect.top });
        }}
        onMouseLeave={() => setCursor(null)}
        onClick={(e) => {
          if (!onPick) return;
          const rect = e.currentTarget.getBoundingClientRect();
          // Simple identity transform — production replaces this with
          // the wgpu camera's screen→world conversion.
          onPick(e.clientX - rect.left, e.clientY - rect.top);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
        }}
        style={{ display: "block", width: "100%", height: "100%" }}
      />
      <div className="draft-canvas__hud" data-testid="draft-canvas-hud">
        <span>Tool: {activeTool}</span>
        {cursor ? (
          <span data-testid="draft-canvas-cursor">
            {Math.round(cursor.x)}, {Math.round(cursor.y)}
          </span>
        ) : null}
        <button
          type="button"
          onClick={() => setSize({ w: size.w + 100, h: size.h })}
          data-testid="draft-canvas-grow"
        >
          +
        </button>
      </div>
    </section>
  );
}
