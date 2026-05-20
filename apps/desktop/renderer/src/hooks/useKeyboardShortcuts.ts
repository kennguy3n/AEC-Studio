/**
 * Global keyboard-shortcut registry.
 *
 * Components register their shortcuts via {@link useShortcut} or by
 * mutating the singleton {@link shortcutRegistry}. A single shell-level
 * `useKeyboardShortcuts` listener walks the registry on every keydown
 * and fires the first matching command's `handler`.
 *
 * Shortcuts are normalised to a stable string form so they can also
 * appear next to commands in the {@link CommandPalette}.
 */
import { useEffect, useRef } from "react";

export interface ShortcutCommand {
  /** Stable id used by the command palette and tests. */
  id: string;
  /** Human-readable label shown in the palette. */
  label: string;
  /** Optional grouping (e.g. "design", "render", "deliver"). */
  group?: string;
  /**
   * Keys in normalised lowercase form, joined by `+`.
   *
   * - `mod` matches Ctrl (Linux/Windows) or Meta/Cmd (macOS).
   * - Letter keys are bare (`s`, `k`).
   * - Special keys use `event.key.toLowerCase()` (`enter`, `escape`, `arrowup`).
   *
   * Example: `mod+s`, `mod+shift+p`, `escape`.
   */
  keys: string;
  /** What runs when the shortcut fires. */
  handler: () => void;
  /**
   * If true, the handler runs even when an input/textarea/contenteditable
   * has focus. Defaults to false so typing in a form doesn't fire
   * `Ctrl+R` and trigger a render.
   */
  whenInputFocused?: boolean;
}

class ShortcutRegistry {
  private commands: ShortcutCommand[] = [];

  register(cmd: ShortcutCommand): () => void {
    // Replace any existing entry with the same id so a remount swaps
    // the handler cleanly instead of stacking duplicates.
    this.commands = this.commands.filter((c) => c.id !== cmd.id);
    this.commands.push(cmd);
    return () => {
      this.commands = this.commands.filter((c) => c.id !== cmd.id);
    };
  }

  all(): readonly ShortcutCommand[] {
    return this.commands;
  }

  findByKeys(keys: string): ShortcutCommand | undefined {
    return this.commands.find((c) => c.keys === keys);
  }

  /** Test helper: wipe the registry between specs. */
  reset(): void {
    this.commands = [];
  }
}

export const shortcutRegistry = new ShortcutRegistry();

/**
 * Normalise a `KeyboardEvent` to the canonical `mod+shift+key` form.
 *
 * We collapse Ctrl and Meta into `mod` so the same shortcut string
 * works on macOS (Cmd) and on Linux/Windows (Ctrl) without forking
 * the registration.
 */
export function eventToKeys(e: KeyboardEvent): string {
  const parts: string[] = [];
  if (e.ctrlKey || e.metaKey) parts.push("mod");
  if (e.shiftKey) parts.push("shift");
  if (e.altKey) parts.push("alt");
  const key = e.key.toLowerCase();
  // Ignore lone modifier presses — they never trigger a shortcut.
  if (key === "control" || key === "meta" || key === "shift" || key === "alt") {
    return "";
  }
  parts.push(key);
  return parts.join("+");
}

function isFormFocus(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName.toLowerCase();
  if (tag === "input" || tag === "textarea" || tag === "select") return true;
  return target.isContentEditable;
}

/**
 * Install the global keydown listener. Mount this once in the app
 * shell — child components register their own commands via
 * {@link useShortcut}.
 */
export function useKeyboardShortcuts(): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const keys = eventToKeys(e);
      if (!keys) return;
      const cmd = shortcutRegistry.findByKeys(keys);
      if (!cmd) return;
      if (!cmd.whenInputFocused && isFormFocus(e.target)) return;
      e.preventDefault();
      cmd.handler();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/**
 * Register a single shortcut for the lifetime of the calling component.
 *
 * **Live-binding semantics.** `id` and `keys` define the registry
 * entry's *identity*; we only re-register when either changes. Every
 * other field (`handler`, `label`, `group`, `whenInputFocused`) is
 * proxied through a ref so callers can flip `whenInputFocused` on the
 * fly, rename a label, or swap an inline handler between renders
 * without losing the registration.
 *
 * The previous implementation only re-routed `handler` through a ref
 * and captured the rest with `...cmd` at registration time. That meant
 * a caller switching e.g. `whenInputFocused: false` → `true` while a
 * form was focused would still be ignored, and the
 * {@link CommandPalette} would keep showing stale labels until the
 * shortcut was actually re-keyed.
 */
export function useShortcut(cmd: ShortcutCommand): void {
  const cmdRef = useRef(cmd);
  cmdRef.current = cmd;

  useEffect(() => {
    return shortcutRegistry.register({
      id: cmd.id,
      keys: cmd.keys,
      get label() {
        return cmdRef.current.label;
      },
      get group() {
        return cmdRef.current.group;
      },
      get whenInputFocused() {
        return cmdRef.current.whenInputFocused;
      },
      handler: () => cmdRef.current.handler(),
    });
    // Identity is `(id, keys)`; non-identity fields are read live via
    // `cmdRef`, so we intentionally exclude them from the dep array.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cmd.id, cmd.keys]);
}
