import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  applyThemeMode,
  parseThemeMode,
  readStoredThemeMode,
  resolveEffectiveTheme,
  THEME_MODES,
  THEME_STORAGE_KEY,
  writeStoredThemeMode,
} from "../lib/theme";

/**
 * Phase 17 Group B Task 8 — theme module coverage.
 *
 * The theme module is a thin layer over `localStorage` +
 * `document.documentElement.dataset.theme` + a `matchMedia` lookup.
 * The tests pin down five contracts:
 *
 *   1. `parseThemeMode` is total over `string | null` (no thrown
 *      errors) and rejects garbage rather than silently coercing.
 *   2. `applyThemeMode("system")` removes the attribute (NOT sets it
 *      to "system") so the CSS cascade falls back to
 *      `@media (prefers-color-scheme: dark)`.
 *   3. The read/write round-trips through localStorage are stable
 *      across every valid mode.
 *   4. `resolveEffectiveTheme` reflects the OS hint only for
 *      mode=system, never for explicit light/dark.
 *   5. Failure of `localStorage` (e.g. quota / private mode) is
 *      swallowed — the helpers never throw, callers never see the
 *      error.
 */

describe("theme parseThemeMode", () => {
  it("accepts all enumerated modes", () => {
    for (const m of THEME_MODES) {
      expect(parseThemeMode(m)).toBe(m);
    }
  });

  it("rejects unknown values without throwing", () => {
    expect(parseThemeMode(null)).toBeNull();
    expect(parseThemeMode("")).toBeNull();
    expect(parseThemeMode("solarized")).toBeNull();
    // Common attempted typos — we intentionally do not coerce.
    expect(parseThemeMode("Dark")).toBeNull();
    expect(parseThemeMode("DARK")).toBeNull();
    expect(parseThemeMode("auto")).toBeNull();
  });
});

describe("theme localStorage round-trip", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("defaults to 'system' when nothing is stored", () => {
    expect(readStoredThemeMode()).toBe("system");
  });

  it("round-trips every valid mode", () => {
    for (const m of THEME_MODES) {
      writeStoredThemeMode(m);
      expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe(m);
      expect(readStoredThemeMode()).toBe(m);
    }
  });

  it("falls back to 'system' on corrupt stored value", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "solarized-pastel");
    expect(readStoredThemeMode()).toBe("system");
  });

  it("swallows localStorage errors silently", () => {
    // Simulate Safari private mode by throwing from setItem.
    const setSpy = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new DOMException("QuotaExceededError");
      });
    expect(() => writeStoredThemeMode("dark")).not.toThrow();
    setSpy.mockRestore();

    const getSpy = vi
      .spyOn(Storage.prototype, "getItem")
      .mockImplementation(() => {
        throw new DOMException("SecurityError");
      });
    expect(readStoredThemeMode()).toBe("system");
    getSpy.mockRestore();
  });
});

describe("theme applyThemeMode", () => {
  afterEach(() => {
    document.documentElement.removeAttribute("data-theme");
  });

  it("removes the attribute for 'system'", () => {
    // Set an explicit theme first so we can prove it's removed.
    document.documentElement.setAttribute("data-theme", "dark");
    applyThemeMode("system");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(
      false,
    );
  });

  it("sets the attribute for explicit modes", () => {
    applyThemeMode("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe(
      "light",
    );
    applyThemeMode("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe(
      "dark",
    );
  });

  it("is idempotent under repeated application", () => {
    applyThemeMode("dark");
    applyThemeMode("dark");
    applyThemeMode("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe(
      "dark",
    );
  });
});

describe("theme resolveEffectiveTheme", () => {
  it("returns the literal for explicit modes", () => {
    // matchMedia returns dark — but the explicit modes ignore it.
    vi.spyOn(window, "matchMedia").mockReturnValue({
      matches: true,
      media: "(prefers-color-scheme: dark)",
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
      onchange: null,
    } as MediaQueryList);
    expect(resolveEffectiveTheme("light")).toBe("light");
    expect(resolveEffectiveTheme("dark")).toBe("dark");
  });

  it("consults matchMedia for 'system'", () => {
    vi.spyOn(window, "matchMedia").mockReturnValue({
      matches: true,
      media: "(prefers-color-scheme: dark)",
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
      onchange: null,
    } as MediaQueryList);
    expect(resolveEffectiveTheme("system")).toBe("dark");

    vi.spyOn(window, "matchMedia").mockReturnValue({
      matches: false,
      media: "(prefers-color-scheme: dark)",
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
      onchange: null,
    } as MediaQueryList);
    expect(resolveEffectiveTheme("system")).toBe("light");
  });
});
