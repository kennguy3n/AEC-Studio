# Contributing to AEC Studio

Thank you for your interest in contributing to AEC Studio! This guide covers everything you need to set up a development environment, build the project, run tests, and submit changes.

---

## Prerequisites

| Tool | Version | Purpose |
|---|---|---|
| **Rust** | 1.75+ (stable) | Core engine, N-API bridge |
| **Node.js** | 20+ | Electron shell, React renderer |
| **npm** | 10+ | Package management |
| **C toolchain** | GCC / Clang / MSVC | Build bundled SQLCipher + OpenSSL |
| **CMake** | 3.22+ | Native dependencies (wgpu native, SQLCipher) |
| **Python** | 3.10+ | Blender worker scripts (only needed when running renders) |

### Platform-specific setup

**Ubuntu / Debian:**

```bash
sudo apt update
sudo apt install build-essential pkg-config libssl-dev cmake python3 python3-pip \
                 libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev \
                 libxi-dev libgl1-mesa-dev libegl1-mesa-dev
```

**macOS:**

```bash
xcode-select --install
brew install cmake python@3.11
```

**Windows:**

Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload, then install [CMake](https://cmake.org/download/) and [Python 3.11+](https://www.python.org/downloads/).

### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
rustup component add clippy rustfmt
```

### Install Node.js

Use [nvm](https://github.com/nvm-sh/nvm) or download directly from [nodejs.org](https://nodejs.org/).

```bash
node --version   # should be 20+
npm --version    # should be 10+
```

---

## Development environment setup

```bash
# Clone the repository
git clone https://github.com/kennguy3n/AEC-Studio.git
cd AEC-Studio

# Install Node.js dependencies
npm install

# Build the Rust crates (first build is slow — compiles bundled SQLCipher + OpenSSL + wgpu native)
cargo build --all-targets

# Build the N-API native addon
npm run build:native
```

---

## Building

### Full build

```bash
npm run build
```

This runs both the Rust build (via napi-rs) and the Electron + Vite build for the renderer.

### Rust only

```bash
cargo build --all-targets
```

### Electron + renderer only

```bash
cd apps/desktop
npm run build
```

---

## Running tests

### All tests

```bash
npm test
```

### Rust tests

```bash
cargo test --all
```

### TypeScript / React tests

```bash
npm run test:ui
```

### Lint and format checks

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings

# TypeScript
npm run lint
npm run type-check
```

---

## Code style

### Rust

- **Formatter:** `rustfmt` — run `cargo fmt --all` before committing.
- **Linter:** `clippy` with `-D warnings` — all warnings are errors in CI.
- **Edition:** 2021, minimum Rust version 1.75 (MSRV).
- Follow the formatting rules in `rustfmt.toml` (matches the [knowledge](https://github.com/kennguy3n/knowledge) substrate):

```toml
edition = "2021"
max_width = 100
hard_tabs = false
tab_spaces = 4
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
```

- Workspace-level clippy lints match the knowledge / Tessera convention:
  - `all = { level = "deny", priority = -1 }`
  - `pedantic = { level = "warn", priority = -1 }`
  - Targeted allows for `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, and other pedantic noise.

### TypeScript / React

- **Formatter:** Prettier — run `npx prettier --write .` to format.
- **Linter:** ESLint — run `npm run lint` to check.
- **Style:** Functional components, hooks, strict TypeScript (`strict: true`).

### General

- No `TODO` or `FIXME` comments in production code — file an issue instead.
- No `any` types in TypeScript.
- No `unsafe` code in Rust (enforced via `unsafe_code = "forbid"` at the workspace level).
- Imports at the top of every file.
- Prefer small, focused PRs that change one logical thing at a time.

---

## PR process

1. **Branch from `main`** — use a descriptive branch name (e.g., `feat/wgpu-viewport-prototype`, `fix/ifc-import-guid`).
2. **Keep commits focused** — one logical change per commit.
3. **Write tests** — every new feature or bug fix should include tests.
4. **Pass CI** — all checks must pass before merge:
   - `cargo fmt --check`
   - `cargo clippy` (warnings as errors)
   - `cargo test --all`
   - `npm run lint`
   - `npm run type-check`
   - `npm test`

   CI runs a **stable Ubuntu-only baseline** on every PR (Rust + TypeScript + Python workers). The full cross-platform matrix (macOS + Windows) runs automatically on every push to `main` and gates the next release. If your PR touches Electron, packaging configs, or OS-specific Rust code and you want the full matrix to run on the PR itself, add the `test-all-platforms` label — the workflow re-fires immediately on `labeled` (you do not need to push another commit), so the next CI run sweeps all three OSes. Removing the label snaps the *next* run back to the Ubuntu-only baseline. This keeps PR turnaround fast and avoids surfacing flakes from platform-specific runners (e.g. Electron CDN 404s on macOS-arm64) on diffs that can't have caused them.
5. **Write a clear PR description** — explain what changed, why, and how to test it.

### Commit message conventions

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): short description

feat(viewport): add wgpu orthographic camera for 2D CAD canvas
fix(bim): preserve unknown Psets on IFC roundtrip
docs(readme): clarify wgpu backend requirements
test(cad): add fixtures for DXF block roundtrip
chore(ci): add macOS runner to CI matrix
refactor(governor): split scheduler from policy
perf(render): cache Blender scene per project
style(rust): apply rustfmt to aec_command
```

**Types:** `feat`, `fix`, `docs`, `test`, `chore`, `refactor`, `perf`, `style`.

---

## Issue reporting

### Bug reports

Open a [GitHub issue](https://github.com/kennguy3n/AEC-Studio/issues) with:

- **Description:** What happened vs. what you expected.
- **Steps to reproduce:** Minimal steps to trigger the bug.
- **Environment:** OS, Rust version, Node.js version, GPU/driver.
- **Logs / screenshots:** Relevant error output, governor state, or screenshots.

### Feature requests

Open an issue with the `enhancement` label:

- **Use case:** What problem does this solve?
- **Proposed solution:** How should it work?
- **Alternatives considered:** What other approaches did you consider?

### Security issues

Do not open public issues for security vulnerabilities — follow the process in [SECURITY.md](SECURITY.md).

---

## Project structure

```
aec-studio/
├── apps/
│   └── desktop/
│       ├── electron/           # Electron main process (main.ts, preload.ts, ipc.ts)
│       └── renderer/           # React / TypeScript UI (pages, components, hooks, styles)
├── crates/                     # Rust core engine
│   ├── aec_core/               # Core types, config, errors, project graph
│   ├── aec_bridge/             # N-API bridge for Electron
│   ├── aec_command/            # Command engine, undo/redo journal
│   ├── aec_geometry/           # Geometry index, spatial queries, mesh cache
│   ├── aec_viewport/           # wgpu viewport, 2D CAD canvas, selection overlays
│   ├── aec_cad/                # 2D CAD: primitives, layers, blocks, snaps, dims
│   ├── aec_bim/                # BIM/IFC: IfcOpenShell adapter, spatial hierarchy
│   ├── aec_render/             # Render queue, Blender/Cycles worker orchestration
│   ├── aec_assets/             # Asset database, import pipeline, LOD, thumbnails
│   ├── aec_materials/          # PBR material library, texture management
│   ├── aec_ai/                 # AI command planner, tool schema, safety validator
│   ├── aec_governor/           # Resource governor, hardware profiler, scheduling
│   ├── aec_export/             # PDF, DXF, IFC, glTF, proposal pack export
│   └── aec_audit/              # Audit trail, project history
├── workers/                    # Native worker processes
│   ├── blender/                # Blender worker scripts (Python)
│   ├── ifc/                    # IfcOpenShell worker
│   └── ai/                     # llama-server sidecar config
├── templates/                  # Project, room, drawing, render, BIM templates
├── assets/                     # Bundled asset packs (furniture, materials, presets)
├── packaging/                  # electron-builder configs (macos, windows)
├── docs/                       # Additional documentation
└── .github/workflows/          # CI configuration
```

---

## License

AGPL-3.0 — see [LICENSE](LICENSE).

By contributing to AEC Studio, you agree that your contributions will be licensed under AGPL-3.0.
