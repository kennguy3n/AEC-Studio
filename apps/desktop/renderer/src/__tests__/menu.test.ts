import { describe, expect, it, vi } from "vitest";
import {
  __buildApplicationMenuTemplate,
  __platformShouldUseMenu,
  installApplicationMenu,
} from "../../../electron/menu";

/**
 * The application menu pins one load-bearing invariant: on macOS
 * the standard `windowMenu` role injects a "Close Window" item
 * with the `CmdOrCtrl+W` accelerator, which Electron processes
 * before keydown events reach the renderer. The renderer
 * registers `mod+w` as the "Close project" shortcut, so any menu
 * binding of `CmdOrCtrl+W` would silently break the project-close
 * UX (Cmd+W would destroy the BrowserWindow instead).
 *
 * These tests verify:
 *   1. macOS gets a custom menu template with the standard
 *      submenus (app, edit, view) and a custom Window submenu
 *      that omits Close Window.
 *   2. The custom Window submenu DOES NOT contain any item with
 *      the `close` role (which is what binds CmdOrCtrl+W) and
 *      DOES NOT contain any item whose `accelerator` is the
 *      `CmdOrCtrl+W` family.
 *   3. The `windowMenu` ROLE is never used at the top level — the
 *      role-based menu is what would auto-bind Close Window.
 *   4. Non-darwin platforms return an empty template, and the
 *      installer passes `null` to `setApplicationMenu` so the
 *      renderer owns every shortcut.
 *   5. The installer routes through the injected `Menu` module so
 *      Electron is not imported at test time.
 */

describe("__platformShouldUseMenu", () => {
  it("returns true for darwin", () => {
    expect(__platformShouldUseMenu("darwin")).toBe(true);
  });

  it("returns false for win32/linux", () => {
    expect(__platformShouldUseMenu("win32")).toBe(false);
    expect(__platformShouldUseMenu("linux")).toBe(false);
  });
});

describe("__buildApplicationMenuTemplate — macOS", () => {
  const template = __buildApplicationMenuTemplate("darwin", "AEC Studio");

  it("includes the standard submenus (app, edit, view, window)", () => {
    // Convert each top-level item to a key we can assert on,
    // tolerant of either a `label` (app menu, custom Window) or a
    // `role` (editMenu / viewMenu).
    const labels = template.map((m) => m.label ?? m.role ?? "");
    expect(labels).toContain("AEC Studio");
    expect(labels).toContain("editMenu");
    expect(labels).toContain("viewMenu");
    expect(labels).toContain("Window");
  });

  it("does NOT use the windowMenu role (which would inject Close Window with CmdOrCtrl+W)", () => {
    const roles = template.map((m) => m.role);
    expect(roles).not.toContain("windowMenu");
  });

  it("Window submenu omits the `close` role", () => {
    const windowMenu = template.find((m) => m.label === "Window");
    expect(windowMenu).toBeDefined();
    const submenu = windowMenu?.submenu;
    expect(Array.isArray(submenu)).toBe(true);
    const items = submenu as Array<{ role?: string }>;
    const roles = items.map((i) => i.role).filter((r) => r !== undefined);
    expect(roles).not.toContain("close");
  });

  it("Window submenu omits any item bound to a CmdOrCtrl+W accelerator", () => {
    const windowMenu = template.find((m) => m.label === "Window");
    const submenu = windowMenu?.submenu as Array<{ accelerator?: string }>;
    for (const item of submenu) {
      const accel = item.accelerator;
      if (typeof accel !== "string") continue;
      // Match CommandOrControl+W / CmdOrCtrl+W / Cmd+W / Ctrl+W
      // (case insensitive). Any of these would intercept the
      // renderer's project-close shortcut.
      expect(accel.toLowerCase()).not.toMatch(/\b(command(or)?control|cmd(or)?ctrl|cmd|ctrl)\+w\b/);
    }
  });

  it("Window submenu retains minimize/zoom/front for macOS parity", () => {
    const windowMenu = template.find((m) => m.label === "Window");
    const submenu = windowMenu?.submenu as Array<{ role?: string }>;
    const roles = submenu.map((i) => i.role).filter((r) => r !== undefined);
    expect(roles).toContain("minimize");
    expect(roles).toContain("zoom");
    expect(roles).toContain("front");
  });
});

describe("__buildApplicationMenuTemplate — non-darwin", () => {
  it("returns an empty template for win32", () => {
    expect(__buildApplicationMenuTemplate("win32", "AEC Studio")).toEqual([]);
  });

  it("returns an empty template for linux", () => {
    expect(__buildApplicationMenuTemplate("linux", "AEC Studio")).toEqual([]);
  });
});

describe("installApplicationMenu", () => {
  it("installs null on win32 so the renderer owns every shortcut", () => {
    const setApplicationMenu = vi.fn();
    const buildFromTemplate = vi.fn();
    installApplicationMenu(
      { setApplicationMenu, buildFromTemplate },
      "win32",
      "AEC Studio",
    );
    expect(setApplicationMenu).toHaveBeenCalledTimes(1);
    expect(setApplicationMenu).toHaveBeenCalledWith(null);
    expect(buildFromTemplate).not.toHaveBeenCalled();
  });

  it("installs null on linux", () => {
    const setApplicationMenu = vi.fn();
    const buildFromTemplate = vi.fn();
    installApplicationMenu(
      { setApplicationMenu, buildFromTemplate },
      "linux",
      "AEC Studio",
    );
    expect(setApplicationMenu).toHaveBeenCalledWith(null);
    expect(buildFromTemplate).not.toHaveBeenCalled();
  });

  it("installs a built menu on darwin and passes the custom Window template", () => {
    const sentinel = { __builtMenu: true } as unknown as Electron.Menu;
    const setApplicationMenu = vi.fn<[Electron.Menu | null], void>();
    const buildFromTemplate = vi.fn<
      [Array<{ label?: string; role?: string }>],
      Electron.Menu
    >(() => sentinel);
    installApplicationMenu(
      // The injected stub matches the structural type the function
      // expects (`Pick<typeof Menu, "buildFromTemplate" | "setApplicationMenu">`),
      // so this cast is purely to satisfy TS's strict structural
      // overload resolution without pulling in the full Electron
      // namespace at test time.
      {
        setApplicationMenu,
        buildFromTemplate,
      } as unknown as Pick<typeof import("electron").Menu, "buildFromTemplate" | "setApplicationMenu">,
      "darwin",
      "AEC Studio",
    );
    expect(buildFromTemplate).toHaveBeenCalledTimes(1);
    const builtTemplate = buildFromTemplate.mock.calls[0][0];
    expect(builtTemplate.map((m) => m.label ?? m.role)).toContain("Window");
    expect(builtTemplate.map((m) => m.role)).not.toContain("windowMenu");
    expect(setApplicationMenu).toHaveBeenCalledWith(sentinel);
  });
});
