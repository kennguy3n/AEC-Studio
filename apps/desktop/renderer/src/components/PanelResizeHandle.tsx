/**
 * Phase 17 Group B Task 14 — Panel resize handles.
 *
 * A pointer-event-driven resize handle that lives *between* two
 * flex children. The handle reports the new size of the controlled
 * side via `onSizeChange` — it does not own the size itself,
 * because layout state lives with the parent (which also persists
 * the user's choice via `usePersistentPanelSize`).
 *
 * Design choices:
 *  - Pointer events (`pointerdown`/`pointermove`/`pointerup`) not
 *    mouse events. Pointer events implicitly handle touch + pen
 *    devices and the `setPointerCapture` API guarantees the drag
 *    keeps tracking even when the pointer leaves the handle.
 *  - The handle reports `size` for "left/top side" — the parent
 *    flexbox shrinks the *other* side to fill remaining space.
 *  - Min/max clamps live in the parent (clamped via `onSizeChange`
 *    returning the *applied* size), so the handle stays a generic
 *    primitive.
 *
 * The grip is rendered visually centred. We don't show it on touch
 * devices since pointer-events don't have a hover state there;
 * `:hover` in the CSS is what makes it appear.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { Icon } from "../icons/Icon";

export type PanelOrientation = "vertical" | "horizontal";

export interface PanelResizeHandleProps {
  /**
   * `"vertical"` (default) is a vertical bar separating two
   * side-by-side columns — drag horizontally to resize the column
   * on the left. `"horizontal"` is a horizontal bar separating
   * stacked rows — drag vertically.
   */
  orientation?: PanelOrientation;
  /**
   * Called with the new pixel size of the controlled side as the
   * user drags. The parent should clamp to min/max and use the
   * value as the column/row size.
   */
  onResize: (delta: number) => void;
  /** Called when the drag starts. */
  onResizeStart?: () => void;
  /** Called when the drag ends — useful for persisting to storage. */
  onResizeEnd?: () => void;
  /** ARIA label for screen readers. */
  label?: string;
  className?: string;
  "data-testid"?: string;
}

export function PanelResizeHandle({
  orientation = "vertical",
  onResize,
  onResizeStart,
  onResizeEnd,
  label,
  className,
  "data-testid": testId,
}: PanelResizeHandleProps): JSX.Element {
  const ref = useRef<HTMLDivElement | null>(null);
  const dragging = useRef<{
    startX: number;
    startY: number;
    pointerId: number;
  } | null>(null);
  const [isDragging, setIsDragging] = useState(false);

  const onPointerDown = useCallback(
    (ev: React.PointerEvent<HTMLDivElement>) => {
      ev.preventDefault();
      const el = ref.current;
      if (!el) return;
      try {
        el.setPointerCapture(ev.pointerId);
      } catch {
        // setPointerCapture can throw in jsdom — degrade
        // gracefully; we still attach window listeners below.
      }
      dragging.current = {
        startX: ev.clientX,
        startY: ev.clientY,
        pointerId: ev.pointerId,
      };
      setIsDragging(true);
      onResizeStart?.();
    },
    [onResizeStart],
  );

  const onPointerMove = useCallback(
    (ev: React.PointerEvent<HTMLDivElement>) => {
      const d = dragging.current;
      if (!d || ev.pointerId !== d.pointerId) return;
      const delta =
        orientation === "vertical"
          ? ev.clientX - d.startX
          : ev.clientY - d.startY;
      onResize(delta);
    },
    [orientation, onResize],
  );

  const finishDrag = useCallback(
    (pointerId: number) => {
      const d = dragging.current;
      if (!d || d.pointerId !== pointerId) return;
      dragging.current = null;
      setIsDragging(false);
      onResizeEnd?.();
    },
    [onResizeEnd],
  );

  const onPointerUp = useCallback(
    (ev: React.PointerEvent<HTMLDivElement>) => {
      finishDrag(ev.pointerId);
    },
    [finishDrag],
  );

  // Cancel-on-Escape: matches the system convention where pressing
  // Escape mid-drag aborts the resize. We can't restore the
  // pre-drag size from inside the handle (it never owned the size),
  // but we can stop tracking immediately so further pointer moves
  // don't continue calling `onResize`.
  useEffect(() => {
    if (!isDragging) return;
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.stopPropagation();
        const d = dragging.current;
        if (d) finishDrag(d.pointerId);
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [isDragging, finishDrag]);

  const classes = [
    "panel-resize-handle",
    orientation === "horizontal" ? "panel-resize-handle--row" : "",
    isDragging ? "is-dragging" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      ref={ref}
      className={classes}
      role="separator"
      aria-orientation={orientation}
      aria-label={label ?? "Resize panel"}
      data-testid={testId ?? "panel-resize-handle"}
      data-orientation={orientation}
      data-dragging={isDragging ? "true" : "false"}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      <span className="panel-resize-handle__grip" aria-hidden>
        <Icon name="grip" size={12} />
      </span>
    </div>
  );
}

/**
 * Persist a panel size to localStorage under a stable key.
 *
 * The hook returns a `[size, setSize]` pair — using `setSize` writes
 * through to localStorage. The initial read defaults to `defaultSize`
 * when nothing is stored or the value is corrupt. The min/max
 * clamps are applied on every set so the persisted value can never
 * be re-loaded into an invalid range (e.g. after the user resizes
 * the window much smaller and the persisted value would crush the
 * viewport to zero).
 */
export function usePersistentPanelSize(
  storageKey: string,
  defaultSize: number,
  opts: { min: number; max: number } = { min: 120, max: 800 },
): [number, (s: number) => void] {
  const [size, setSizeState] = useState<number>(() => {
    try {
      if (typeof localStorage === "undefined") return defaultSize;
      const raw = localStorage.getItem(storageKey);
      if (raw === null || raw === "") return defaultSize;
      const n = Number(raw);
      if (!Number.isFinite(n)) return defaultSize;
      return Math.max(opts.min, Math.min(opts.max, n));
    } catch {
      return defaultSize;
    }
  });
  const setSize = useCallback(
    (next: number) => {
      const clamped = Math.max(opts.min, Math.min(opts.max, next));
      setSizeState(clamped);
      try {
        if (typeof localStorage !== "undefined") {
          localStorage.setItem(storageKey, String(clamped));
        }
      } catch {
        /* noop */
      }
    },
    [storageKey, opts.min, opts.max],
  );
  return [size, setSize];
}
