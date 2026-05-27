import { useEffect, useState } from "react";
import {
  shortcutRegistry,
  type ShortcutCommand,
  useShortcut,
} from "../hooks/useKeyboardShortcuts";

/**
 * Help overlay that lists every registered keyboard shortcut grouped
 * by the `group` field of {@link ShortcutCommand}. Triggered via
 * `?` (Shift+/) which is conventional across most desktop apps.
 *
 * Reads from {@link shortcutRegistry} so the list is always live —
 * adding a new `useShortcut(...)` somewhere in the tree means the
 * help overlay reflects it without any registration boilerplate
 * here.
 */
export function ShortcutHelp(): JSX.Element | null {
  const [open, setOpen] = useState(false);

  // Browsers deliver Shift+/ as `key === "?"` with `shiftKey === true`,
  // so the normalised chord is `shift+?` (the registry preserves the
  // shift token even when the resulting key is already a shifted
  // character). Register the canonical form here.
  useShortcut({
    id: "shortcut-help",
    label: "Show keyboard shortcuts",
    group: "global",
    keys: "shift+?",
    handler: () => setOpen((prev) => !prev),
    whenInputFocused: false,
  });

  // Ctrl/Cmd+/ alternative for keyboards / IMEs where `?` doesn't
  // surface cleanly through `event.key`.
  useShortcut({
    id: "shortcut-help-alt",
    label: "Show keyboard shortcuts (alt)",
    group: "global",
    keys: "mod+/",
    handler: () => setOpen((prev) => !prev),
    whenInputFocused: false,
  });

  // Snapshot shortcuts when the overlay opens so the list doesn't
  // mutate underneath the user as they read it. We re-snapshot on
  // each open so newly-registered shortcuts appear next time.
  const [shortcuts, setShortcuts] = useState<readonly ShortcutCommand[]>([]);
  useEffect(() => {
    if (open) {
      setShortcuts([...shortcutRegistry.all()]);
    }
  }, [open]);

  if (!open) return null;

  const grouped = groupBy(shortcuts);

  return (
    <div
      className="shortcut-help"
      role="dialog"
      aria-modal="true"
      aria-label="Keyboard shortcuts"
      data-testid="shortcut-help"
    >
      <div className="shortcut-help__panel">
        <header className="shortcut-help__header">
          <h2>Keyboard Shortcuts</h2>
          <button
            type="button"
            onClick={() => setOpen(false)}
            aria-label="Close shortcut help"
            data-testid="shortcut-help-close"
          >
            ×
          </button>
        </header>
        <div className="shortcut-help__body">
          {grouped.length === 0 ? (
            <p>No shortcuts registered.</p>
          ) : (
            grouped.map(([groupName, items]) => (
              <section
                key={groupName}
                className="shortcut-help__group"
                data-testid={`shortcut-group-${groupName}`}
              >
                <h3>{groupName}</h3>
                <ul>
                  {items.map((s) => (
                    <li key={s.id}>
                      <kbd className="shortcut-help__keys">
                        {formatKeys(s.keys)}
                      </kbd>
                      <span className="shortcut-help__label">{s.label}</span>
                    </li>
                  ))}
                </ul>
              </section>
            ))
          )}
        </div>
      </div>
      <div
        className="shortcut-help__backdrop"
        onClick={() => setOpen(false)}
        aria-hidden="true"
      />
    </div>
  );
}

function groupBy(
  shortcuts: readonly ShortcutCommand[],
): Array<[string, ShortcutCommand[]]> {
  const map = new Map<string, ShortcutCommand[]>();
  for (const s of shortcuts) {
    const key = s.group ?? "other";
    const list = map.get(key) ?? [];
    list.push(s);
    map.set(key, list);
  }
  return [...map.entries()].sort(([a], [b]) => a.localeCompare(b));
}

function formatKeys(keys: string): string {
  return keys
    .split("+")
    .map((p) => {
      if (p === "mod") return "Ctrl";
      if (p === "shift") return "Shift";
      if (p === "alt") return "Alt";
      if (p.length === 1) return p.toUpperCase();
      return p.charAt(0).toUpperCase() + p.slice(1);
    })
    .join(" + ");
}
