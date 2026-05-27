/**
 * Application menu construction.
 *
 * Electron creates a default application menu when none is set. On
 * macOS that default menu includes a `windowMenu` role whose
 * standard items inject "Close Window" with the `CmdOrCtrl+W`
 * accelerator. Electron processes menu accelerators *before*
 * keydown events reach the renderer, so any renderer-side
 * `mod+w` shortcut never fires — pressing Cmd+W triggers
 * `BrowserWindow.close()` instead.
 *
 * The renderer registers `mod+w` as the "Close project" shortcut
 * (see `apps/desktop/renderer/src/App.tsx`, `close-project`
 * `useShortcut`): the intended UX is to close the active project
 * and return to Home, NOT to destroy the BrowserWindow. To make
 * that contract hold, the main process owns menu construction
 * end-to-end:
 *
 *   * On macOS we build a custom menu that keeps the standard
 *     conventions users expect (the app menu with About / Hide /
 *     Quit, the Edit menu so Cmd+C/V/A still work in input fields,
 *     the View menu for the dev/devtools shortcuts) but
 *     deliberately replaces the standard `windowMenu` role with a
 *     custom Window submenu that omits "Close Window". The
 *     `CmdOrCtrl+W` accelerator therefore has no menu binding and
 *     propagates to the renderer.
 *   * On Windows / Linux there is no native menu-bar convention
 *     we want to keep — every shortcut goes through the renderer's
 *     command palette and `useShortcut` registry, so we suppress
 *     the auto-menu entirely with a `null` application menu.
 *
 * Split out into its own module (rather than inlining in `main.ts`)
 * so a vitest can exercise the platform branches without spinning
 * up Electron: `__buildApplicationMenuTemplate` and
 * `__platformShouldUseMenu` are deliberately exported as internals.
 */

import { Menu, app, type MenuItemConstructorOptions } from "electron";

/**
 * Whether the current platform should install an application menu.
 * macOS users expect a menu bar; other platforms get `null` so
 * the renderer owns every shortcut.
 *
 * Pure function (takes platform as a parameter) so tests can drive
 * both branches without touching `process.platform`.
 */
export function __platformShouldUseMenu(platform: NodeJS.Platform): boolean {
  return platform === "darwin";
}

/**
 * Build the menu template for the given platform.
 *
 *   * `darwin` — full custom template with app/edit/view/window/help
 *     submenus; the Window submenu deliberately omits the standard
 *     "Close Window" item so `CmdOrCtrl+W` propagates to the
 *     renderer's `close-project` shortcut.
 *   * Anything else — empty array, signalling the caller to install
 *     `null` (no menu).
 *
 * `appName` is injected (rather than read from `app.getName()`)
 * so the function stays pure and testable.
 */
export function __buildApplicationMenuTemplate(
  platform: NodeJS.Platform,
  appName: string,
): MenuItemConstructorOptions[] {
  if (platform !== "darwin") {
    return [];
  }
  return [
    {
      label: appName,
      submenu: [
        { role: "about" },
        { type: "separator" },
        { role: "services" },
        { type: "separator" },
        { role: "hide" },
        { role: "hideOthers" },
        { role: "unhide" },
        { type: "separator" },
        { role: "quit" },
      ],
    },
    { role: "editMenu" },
    { role: "viewMenu" },
    {
      // Custom Window submenu. We intentionally do NOT use the
      // `windowMenu` role — that role injects a standard "Close
      // Window" item with the `CmdOrCtrl+W` accelerator, which is
      // exactly the conflict we are guarding against. Minimize /
      // Zoom / Bring All to Front are retained for parity with
      // macOS conventions.
      label: "Window",
      submenu: [
        { role: "minimize" },
        { role: "zoom" },
        { type: "separator" },
        { role: "front" },
      ],
    },
  ];
}

/**
 * Install the application menu (or clear it on non-macOS).
 *
 * Idempotent — calling this more than once is safe (subsequent
 * calls overwrite the previous menu). The `Menu` reference is
 * injected so tests can pass a stub without importing the real
 * Electron `Menu` namespace.
 */
export function installApplicationMenu(
  menuModule: Pick<typeof Menu, "buildFromTemplate" | "setApplicationMenu"> = Menu,
  platform: NodeJS.Platform = process.platform,
  appName: string = app.getName(),
): void {
  if (!__platformShouldUseMenu(platform)) {
    menuModule.setApplicationMenu(null);
    return;
  }
  const template = __buildApplicationMenuTemplate(platform, appName);
  const menu = menuModule.buildFromTemplate(template);
  menuModule.setApplicationMenu(menu);
}
