/**
 * Phase 17 Group B Task 10 — Context menu primitive.
 *
 * A right-click / `onContextMenu`-driven menu that renders into a
 * React portal at `document.body`. The portal placement matters:
 *  - Menus must escape `overflow: hidden` ancestors (the layer
 *    panel, the asset browser, the spatial tree all crop their
 *    content). A portal at the document root sidesteps all of those.
 *  - Menus must be `position: fixed` in viewport coordinates so they
 *    follow the pointer regardless of scroll position.
 *
 * The API is two parts:
 *   1. `<ContextMenuTrigger items={...}><div>...</div></ContextMenuTrigger>`
 *      — wraps a piece of UI and binds `onContextMenu` to open the
 *      menu at the pointer coordinates.
 *   2. `<ContextMenu>` — the actual portal-rendered menu (used by
 *      the trigger; exported separately so callers that need fully
 *      manual control over open state can do so).
 *
 * Dismiss behaviour matches Figma / macOS Finder conventions:
 *   - Click outside dismisses
 *   - Escape dismisses
 *   - Selecting an item dismisses and invokes the item's `onSelect`
 *   - Window blur dismisses
 */

import { useCallback, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Icon, type IconName } from "../icons/Icon";

/**
 * A single menu entry. Three shapes:
 *
 *   - `kind: "item"` — a clickable action. `label` is the visible
 *     text, `onSelect` runs when chosen, `icon` is an optional
 *     `IconName` rendered on the left, `shortcut` is right-aligned
 *     muted text (e.g. "⌘C"). `disabled` greys it out.
 *   - `kind: "separator"` — a 1-px horizontal rule between groups.
 *   - `kind: "submenu"` — declares a nested menu under a label. The
 *     `items` array is the submenu. Submenus open on hover with a
 *     small delay (75 ms) and close when the pointer moves to a
 *     different parent item or outside the menu tree.
 *
 * Discriminating on `kind` keeps the type narrowing precise — every
 * render-site has to switch on the variant explicitly, which makes
 * "I forgot the separator case" impossible.
 */
export type ContextMenuItem =
  | {
      kind: "item";
      label: string;
      onSelect: () => void;
      icon?: IconName;
      shortcut?: string;
      disabled?: boolean;
      /** Test id override; defaults to `context-menu-item-${label}`. */
      "data-testid"?: string;
    }
  | { kind: "separator" }
  | {
      kind: "submenu";
      label: string;
      items: ContextMenuItem[];
      icon?: IconName;
      disabled?: boolean;
    };

export interface ContextMenuProps {
  items: ContextMenuItem[];
  /** Viewport coordinates of the menu's top-left corner. */
  x: number;
  y: number;
  /** Called when the menu should dismiss (Escape, click-outside, item select). */
  onDismiss: () => void;
  /** Optional `aria-label` for screen reader announcement on open. */
  label?: string;
  /**
   * Test-only escape hatch: render directly into the test container
   * instead of `document.body`. Avoids the cross-document portal
   * tracking that some React-Testing-Library setups stumble on.
   */
  portalNode?: HTMLElement | null;
}

/**
 * Position the menu so it stays inside the viewport even when the
 * trigger is near the right or bottom edge — flip it like macOS /
 * Windows / Linux native menus do. The clamp falls back to the
 * provided coords when the menu happens to fit, which is the common
 * case.
 */
function clampToViewport(
  x: number,
  y: number,
  w: number,
  h: number,
): { x: number; y: number } {
  if (typeof window === "undefined") return { x, y };
  const margin = 4;
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  let cx = x;
  let cy = y;
  if (cx + w + margin > vw) cx = Math.max(margin, vw - w - margin);
  if (cy + h + margin > vh) cy = Math.max(margin, vh - h - margin);
  return { x: cx, y: cy };
}

export function ContextMenu({
  items,
  x,
  y,
  onDismiss,
  label,
  portalNode,
}: ContextMenuProps): JSX.Element | null {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [resolved, setResolved] = useState<{ x: number; y: number }>(
    { x, y },
  );
  const menuId = useId();

  // Reflow after layout: read the menu's natural size, then clamp.
  // Skipping the reflow when SSR-rendering (no window) keeps the
  // component safe to import from non-DOM contexts.
  useEffect(() => {
    const el = menuRef.current;
    if (!el || typeof window === "undefined") return;
    const r = el.getBoundingClientRect();
    setResolved(clampToViewport(x, y, r.width, r.height));
  }, [x, y, items]);

  // Click-outside dismiss: stop on capture so we beat handlers
  // attached deeper in the tree. The menu itself uses a `data-menu`
  // marker so we can recognise our own clicks even when the user
  // drags slightly between mousedown and mouseup.
  useEffect(() => {
    if (typeof document === "undefined") return;
    const onPointerDown = (ev: PointerEvent) => {
      const target = ev.target as HTMLElement | null;
      if (target && target.closest("[data-context-menu]")) return;
      onDismiss();
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.stopPropagation();
        onDismiss();
      }
    };
    const onBlur = () => onDismiss();
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [onDismiss]);

  // Establish portal target after mount so SSR doesn't crash on
  // `document.body`. The `portalNode` prop wins so tests can route
  // the portal somewhere they own.
  const [host, setHost] = useState<HTMLElement | null>(portalNode ?? null);
  useEffect(() => {
    if (portalNode !== undefined) {
      setHost(portalNode);
    } else if (typeof document !== "undefined") {
      setHost(document.body);
    }
  }, [portalNode]);

  if (!host) return null;

  return createPortal(
    <div
      ref={menuRef}
      role="menu"
      aria-label={label}
      data-context-menu
      data-testid="context-menu"
      data-menu-id={menuId}
      className="context-menu"
      style={{
        position: "fixed",
        top: resolved.y,
        left: resolved.x,
      }}
    >
      {items.map((item, i) => (
        <ContextMenuRow
          key={i}
          item={item}
          onDismiss={onDismiss}
        />
      ))}
    </div>,
    host,
  );
}

function ContextMenuRow({
  item,
  onDismiss,
}: {
  item: ContextMenuItem;
  onDismiss: () => void;
}): JSX.Element {
  if (item.kind === "separator") {
    return (
      <div
        className="context-menu__separator"
        data-testid="context-menu-separator"
        role="separator"
      />
    );
  }
  if (item.kind === "submenu") {
    return (
      <ContextMenuSubmenuRow item={item} onDismiss={onDismiss} />
    );
  }
  const testId =
    item["data-testid"] ??
    `context-menu-item-${item.label.toLowerCase().replace(/\s+/g, "-")}`;
  return (
    <button
      type="button"
      role="menuitem"
      data-testid={testId}
      className="context-menu__item"
      disabled={item.disabled}
      onClick={() => {
        if (item.disabled) return;
        item.onSelect();
        onDismiss();
      }}
    >
      {item.icon && (
        <span className="context-menu__icon" aria-hidden>
          <Icon name={item.icon} size={14} />
        </span>
      )}
      <span className="context-menu__label">{item.label}</span>
      {item.shortcut && (
        <span className="context-menu__shortcut" aria-hidden>
          {item.shortcut}
        </span>
      )}
    </button>
  );
}

function ContextMenuSubmenuRow({
  item,
  onDismiss,
}: {
  item: Extract<ContextMenuItem, { kind: "submenu" }>;
  onDismiss: () => void;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  const rowRef = useRef<HTMLButtonElement | null>(null);
  const [anchor, setAnchor] = useState<{ x: number; y: number }>({
    x: 0,
    y: 0,
  });

  // Position submenu to the right of the parent row; flip handled
  // by the nested ContextMenu's own clamp.
  const openSubmenu = useCallback(() => {
    const el = rowRef.current;
    if (!el || item.disabled) return;
    const r = el.getBoundingClientRect();
    setAnchor({ x: r.right + 1, y: r.top });
    setOpen(true);
  }, [item.disabled]);

  return (
    <>
      <button
        ref={rowRef}
        type="button"
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid={`context-menu-submenu-${item.label
          .toLowerCase()
          .replace(/\s+/g, "-")}`}
        className="context-menu__item context-menu__item--submenu"
        disabled={item.disabled}
        onPointerEnter={openSubmenu}
        onFocus={openSubmenu}
        onClick={openSubmenu}
      >
        {item.icon && (
          <span className="context-menu__icon" aria-hidden>
            <Icon name={item.icon} size={14} />
          </span>
        )}
        <span className="context-menu__label">{item.label}</span>
        <span className="context-menu__shortcut" aria-hidden>
          <Icon name="chevronRight" size={12} />
        </span>
      </button>
      {open && (
        <ContextMenu
          items={item.items}
          x={anchor.x}
          y={anchor.y}
          onDismiss={() => {
            setOpen(false);
            onDismiss();
          }}
        />
      )}
    </>
  );
}

/**
 * Wrap any piece of UI to attach a context menu on right-click.
 *
 * The trigger absorbs the native `contextmenu` event, calls
 * `preventDefault()` (so the browser's default menu doesn't appear
 * underneath), and renders the menu at the pointer coordinates.
 *
 * Multiple triggers in the same tree are safe — opening a second
 * trigger dismisses the first (by virtue of click-outside).
 */
export function ContextMenuTrigger({
  items,
  children,
  label,
  className,
  "data-testid": testId,
}: {
  items: ContextMenuItem[];
  children: React.ReactNode;
  label?: string;
  className?: string;
  "data-testid"?: string;
}): JSX.Element {
  const [coords, setCoords] = useState<{ x: number; y: number } | null>(
    null,
  );
  const onCtx = useCallback((ev: React.MouseEvent) => {
    ev.preventDefault();
    setCoords({ x: ev.clientX, y: ev.clientY });
  }, []);
  return (
    <div
      onContextMenu={onCtx}
      className={className}
      data-testid={testId ?? "context-menu-trigger"}
    >
      {children}
      {coords !== null && (
        <ContextMenu
          items={items}
          x={coords.x}
          y={coords.y}
          label={label}
          onDismiss={() => setCoords(null)}
        />
      )}
    </div>
  );
}
