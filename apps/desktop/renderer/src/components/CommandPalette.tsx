/**
 * Ctrl/Cmd+K command palette.
 *
 * Renders a modal overlay listing every command from
 * {@link shortcutRegistry}, fuzzy-filtered by the search input. Hitting
 * Enter fires the highlighted command's handler.
 *
 * Visibility is controlled by the parent — usually via a global
 * `Ctrl+K` shortcut wired from `App.tsx`. The palette is testable in
 * isolation because the registry is a module-level singleton.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import {
  shortcutRegistry,
  type ShortcutCommand,
} from "../hooks/useKeyboardShortcuts";

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  /**
   * Optional list of commands to render. Defaults to every command
   * currently in the registry. The override exists so tests can
   * exercise the filtering logic without first wiring shortcuts.
   */
  commands?: readonly ShortcutCommand[];
}

function fuzzyScore(haystack: string, needle: string): number {
  if (!needle) return 1;
  const h = haystack.toLowerCase();
  const n = needle.toLowerCase();
  // Exact substring match wins.
  if (h.includes(n)) return 1 + n.length;
  // Subsequence match: every needle char appears in order.
  let hi = 0;
  for (const ch of n) {
    const idx = h.indexOf(ch, hi);
    if (idx < 0) return 0;
    hi = idx + 1;
  }
  return n.length / (h.length + 1);
}

export function filterCommands(
  commands: readonly ShortcutCommand[],
  query: string,
): ShortcutCommand[] {
  if (!query) return [...commands];
  return commands
    .map((cmd) => {
      const labelScore = fuzzyScore(cmd.label, query);
      const idScore = fuzzyScore(cmd.id, query) * 0.5;
      const groupScore = cmd.group ? fuzzyScore(cmd.group, query) * 0.3 : 0;
      return { cmd, score: Math.max(labelScore, idScore, groupScore) };
    })
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((entry) => entry.cmd);
}

export function CommandPalette({ open, onClose, commands }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Snapshot the registry whenever the palette opens. If a parent
  // passes `commands` we honour that, otherwise we read the singleton.
  // `open` is intentionally a dependency so that closing-then-reopening
  // the palette picks up commands registered while it was hidden.
  const all = useMemo<readonly ShortcutCommand[]>(
    () => commands ?? shortcutRegistry.all(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [commands, open],
  );

  const matches = useMemo(() => filterCommands(all, query), [all, query]);

  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
      // Focus the input so the user can start typing immediately.
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  // Clamp active index when the result list shrinks.
  useEffect(() => {
    if (active >= matches.length) {
      setActive(Math.max(0, matches.length - 1));
    }
  }, [matches.length, active]);

  if (!open) return null;

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(matches.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = matches[active];
      if (cmd) {
        cmd.handler();
        onClose();
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div
      className="command-palette-overlay"
      data-testid="command-palette-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      onClick={onClose}
    >
      <div
        className="command-palette"
        data-testid="command-palette"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          type="text"
          className="command-palette__input"
          data-testid="command-palette-input"
          placeholder="Type a command…"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
          onKeyDown={onKeyDown}
          aria-label="Filter commands"
        />
        <ul
          className="command-palette__results"
          data-testid="command-palette-results"
        >
          {matches.length === 0 && (
            <li
              className="command-palette__empty"
              data-testid="command-palette-empty"
            >
              No commands match “{query}”
            </li>
          )}
          {matches.map((cmd, i) => (
            <li
              key={cmd.id}
              data-testid={`command-palette-item-${cmd.id}`}
              className={
                i === active
                  ? "command-palette__item is-active"
                  : "command-palette__item"
              }
              onMouseEnter={() => setActive(i)}
              onClick={() => {
                cmd.handler();
                onClose();
              }}
            >
              <span className="command-palette__label">{cmd.label}</span>
              <span className="command-palette__keys" aria-hidden>
                {cmd.keys}
              </span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

export default CommandPalette;
