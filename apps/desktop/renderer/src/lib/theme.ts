/**
 * Phase 17 Group B Task 8 — theme management.
 *
 * Three modes, mirroring macOS / Windows / GNOME conventions:
 *   - "system": follow `prefers-color-scheme` (the default — first
 *     launch has no opinion, takes the OS hint)
 *   - "light": explicit light, overrides the OS
 *   - "dark":  explicit dark, overrides the OS
 *
 * The preference persists in `localStorage` under `aec.theme.mode`
 * across sessions. We apply it to the document by setting
 * `data-theme` on `<html>`:
 *   - mode=light → `<html data-theme="light">` → forces the light
 *     palette even when the OS reports dark.
 *   - mode=dark  → `<html data-theme="dark">`  → forces dark.
 *   - mode=system → no attribute → tokens.css's
 *     `@media (prefers-color-scheme: dark)` rule activates iff the
 *     OS is dark.
 *
 * The "no attribute for system" choice (vs. setting
 * `data-theme="system"`) matters for SSR-style first-paint: a flash
 * of light → dark happens when the document renders before the
 * theme hook runs. By keeping `system` attribute-free, the cascade
 * defaults to the OS hint from the very first paint — no flash.
 *
 * The hook also subscribes to `prefers-color-scheme` changes so
 * `mode=system` users automatically follow the OS dark/light toggle
 * without restarting the app. This is the de-facto behavior of
 * Slack / VS Code / Figma desktop and is what users expect.
 */

export type ThemeMode = "system" | "light" | "dark";

/**
 * Key used in `localStorage`. Namespaced so it doesn't collide with
 * other parts of the app and so a future "reset preferences" flow
 * can clear all `aec.*` keys atomically.
 */
export const THEME_STORAGE_KEY = "aec.theme.mode";

/** All valid `ThemeMode` values — useful for tests and Settings UI. */
export const THEME_MODES: readonly ThemeMode[] = [
  "system",
  "light",
  "dark",
] as const;

/**
 * Type guard that narrows a free-form `string | null` from
 * `localStorage.getItem(...)` into a `ThemeMode`. Returns `null`
 * if the stored value is missing, empty, or unrecognised — the
 * caller is expected to fall back to the default ("system").
 */
export function parseThemeMode(raw: string | null): ThemeMode | null {
  if (raw === null || raw === "") return null;
  return (THEME_MODES as readonly string[]).includes(raw)
    ? (raw as ThemeMode)
    : null;
}

/**
 * Read the user's last-saved theme preference. Defaults to
 * "system" when nothing is stored or the value is corrupt.
 *
 * Safe to call from SSR / vitest jsdom: if `localStorage` throws
 * (e.g. Safari private mode, sandboxed iframe), the catch returns
 * the default.
 */
export function readStoredThemeMode(): ThemeMode {
  try {
    const raw =
      typeof localStorage !== "undefined"
        ? localStorage.getItem(THEME_STORAGE_KEY)
        : null;
    return parseThemeMode(raw) ?? "system";
  } catch {
    return "system";
  }
}

/**
 * Persist the user's theme preference.
 *
 * Failures are swallowed: localStorage quota exhaustion or sandboxed
 * iframes shouldn't crash the Settings page; the in-memory React
 * state still drives the live preview correctly.
 */
export function writeStoredThemeMode(mode: ThemeMode): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(THEME_STORAGE_KEY, mode);
    }
  } catch {
    /* noop */
  }
}

/**
 * Apply a theme mode to the document. Idempotent — calling this
 * with the same `mode` repeatedly is a no-op (the DOM attribute
 * read is cheap).
 *
 *   "system" → remove the `data-theme` attribute, let the
 *              `@media (prefers-color-scheme: dark)` rule decide.
 *   "light"  → `data-theme="light"` (overrides OS dark).
 *   "dark"   → `data-theme="dark"`  (forces dark on every OS).
 *
 * Exported separately from the React hook so tests and the
 * `index.html` bootstrap script can call it without mounting React.
 */
export function applyThemeMode(mode: ThemeMode): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (mode === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", mode);
  }
}

/**
 * Compute the *effective* theme — the actual light/dark the user is
 * looking at right now, after resolving "system" against the OS.
 *
 * Used by feature code that needs the resolved value (e.g. a canvas
 * that draws gridlines in a different color per theme) and by tests
 * asserting the cascade is right.
 */
export function resolveEffectiveTheme(
  mode: ThemeMode,
): "light" | "dark" {
  if (mode === "light" || mode === "dark") return mode;
  if (typeof window === "undefined" || !window.matchMedia) return "light";
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}
