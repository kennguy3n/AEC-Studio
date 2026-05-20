# Linux packaging

This directory holds the electron-builder configuration for Linux
desktop builds of AEC Studio.

## Targets

* **AppImage** — the primary distribution artifact; portable, runs on
  any glibc ≥ 2.35 distro without root.
* **deb** — Debian / Ubuntu native package; declares its native deps so
  apt resolves them at install time.
* **snap** — confined Snap Store package (optional).

## Building locally

```bash
# 1) Build the renderer + electron bundles
cd apps/desktop
npm install
npm run build

# 2) Run electron-builder pointing at this config
npx electron-builder --linux \
  --config ../../packaging/linux/electron-builder.linux.yml \
  --projectDir .
```

Artifacts are written to `apps/desktop/dist/installers/linux/`.

## Native runtime dependencies

The Electron shell relies on the same GTK + NSS stack that Chrome uses.
On Ubuntu 22.04+ the AppImage and `.deb` runtime needs:

* `libgtk-3-0`
* `libnss3`
* `libxss1`
* `libasound2`
* `libnotify4`

The `deb` target's `depends` block already declares these so `apt install`
pulls them in. AppImage users typically have them already; the [Linux
prerequisites](../../README.md#linux-prerequisites) section of the
project README spells the install command out.

## Icons

Icon variants live in `resources/icons/`. We ship 16, 32, 48, 64, 128,
256, and 512 px PNGs plus a 512 px SVG; electron-builder picks the
correct sizes for the AppImage and `.desktop` file automatically.

The icons are placeholders for now — to replace them, drop new PNGs
matching the existing names into `resources/icons/` and re-run the
build. SVGs must be flattened before they ship.

## CI

The `Package (Linux)` job in `.github/workflows/ci.yml` runs
`npx electron-builder --linux` against this config on every push to
`main` and uploads the AppImage + `.deb` artifacts.
