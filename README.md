# AEC Studio

> AEC Studio is a local-first open-source desktop suite for architecture, interior design, and construction — covering the daily production loop from plan to model to render to draft to deliver.

---

## What AEC Studio does

- **ArchViz / Interior Studio** — model rooms, walls, floors, ceilings, doors, and windows; place furniture from a curated asset library; assign PBR materials; render photorealistic interiors locally.
- **2D CAD Drafting** — produce real construction documentation: floor plans, sections, elevations, details, schedules, title blocks, and printable sheets.
- **BIM Lite / IFC** — import, view, classify, and lightly edit IFC building models; produce room and door/window schedules; do quantity takeoff for small projects.
- **Asset Library** — bundled and importable furniture, materials, and presets organized by project type (apartment, café, office, villa, retail, kitchen, bathroom, renovation).
- **Local Render** — PBR-class previews and photorealistic final renders driven by a native Rust path tracer (CPU + wgpu compute), with batch queues, presets, and walkthrough/panorama support. No external runtime needed.
- **Local AI** — on-device AI assistants for plan detection, style suggestions, render-doctor diagnostics, CAD cleanup, and BIM classification — all running through a local llama.cpp / PrismML sidecar with explicit, previewable actions.
- **Deliver** — proposal packs, contractor handoff bundles, BOQ-lite quantity exports, IFC packs, and PDF/DXF exports tailored to the project type.
- **One project, many outputs** — a single `.aecstudio` package emits client decks, drawings, BIM exports, and contractor packages without duplicating data.

## What AEC Studio is not

- **Not a clone of AutoCAD, Revit, or SketchUp** — it is a focused, opinionated suite for the production loop of small studios and freelancers, not a general-purpose CAD/BIM platform.
- **Not a cloud-dependent SaaS** — every project, every asset, every render, and every AI inference runs on your machine by default. There is no cloud backend, no telemetry, and no remote rendering required.
- **Not a general chatbot** — AI is scoped to design, drafting, and BIM tools with a strict tool schema and a safety validator. There is no free-form chat surface, no internet retrieval, and no silent geometry mutation.
- **Not a real-time game engine** — the viewport prioritizes accuracy, snapping, and predictable performance over framerate. Final rendering uses the in-process Rust path tracer (wgpu compute + CPU fallback).
- **Not a clipper for stock-photo VizPacks** — bundled assets are deliberate, tagged, and license-clean. Users curate their own asset library on top.

---

## Core principles

1. **Local-first by default** — projects, assets, models, renders, and AI inference live on your machine; nothing leaves without an explicit export or sync action.
2. **Template-first** — every workflow starts from a real, opinionated template (apartment, office, villa, café, retail, kitchen, bathroom, renovation). Empty canvases are the exception, not the rule.
3. **Mode-based UX** — Home, Design, Draft, BIM, Render, and Deliver are first-class workflow modes. The toolbar, inspectors, and AI panel adapt to the active mode.
4. **Progressive disclosure** — beginners see a clean, opinionated UI; advanced controls (snaps, constraints, command line, render presets, governor overrides) are reachable but never forced.
5. **One project, many outputs** — a single project package feeds client renders, drawing sets, BIM exports, and contractor handoff packs.
6. **AI is inspectable** — every AI action is shown as a previewable diff, scoped through a typed tool schema, and recorded in the audit trail. AI never silently mutates geometry.
7. **Performance is visible** — the resource governor and hardware profile are first-class: users always know what is running, where it is running (CPU/GPU/Apple Silicon), and why.

---

## Platforms

| Platform | Status | Installers |
|---|---|---|
| macOS (Intel & Apple Silicon) | Alpha | `.dmg` + `.zip` (universal binary, hardened runtime) |
| Windows (x64) | Alpha | `.exe` (NSIS) + `.msi` |
| Linux (x64) | Alpha | `.AppImage` + `.deb` + `.snap` |

Desktop only. Supports **CPU-only** and **CPU+GPU** configurations.

### Local optimization

| Target | Acceleration |
|---|---|
| Apple Silicon (macOS) | **MLX** for inference, Metal for viewport and the native wgpu path tracer |
| Windows CPU | **llama.cpp** (PrismML fork) with **AVX2 / AVX-VNNI / AVX-512 VNNI** |
| Windows GPU | **Vulkan / CUDA** for inference, DX12 / Vulkan for the native wgpu path tracer |
| Linux CPU | **llama.cpp** (PrismML fork) with **AVX2 / AVX-VNNI / AVX-512 VNNI** |
| Linux GPU | **Vulkan** for inference and the native wgpu path tracer (CUDA optional on NVIDIA for inference only) |
| Viewport / CAD canvas (all platforms) | **wgpu** with Vulkan, Metal, D3D12, or OpenGL backend |

---

## Stack summary

| Layer | Technology |
|---|---|
| Desktop shell | Electron |
| UI framework | React + TypeScript |
| Core engine | Rust |
| Native viewport | wgpu (Vulkan / Metal / D3D12 / OpenGL) |
| Local storage | SQLite / SQLCipher |
| Model runtime | llama.cpp / PrismML sidecar |
| Apple Silicon acceleration | MLX |
| BIM / IFC | Native Rust STEP parser + writer + tessellator (`aec_bim::ifc`) — IFC4 with IFC2x3 / IFC4x3 input compatibility |
| Render engine | Native Rust path tracer + PBR rasterizer (wgpu compute, CPU fallback) |
| Electron ↔ Rust bridge | N-API (napi-rs) |
| Packaging | electron-builder |

---

## Architecture overview

AEC Studio is structured as an Electron desktop application with a React/TypeScript renderer, a Rust core engine accessed via N-API, a cross-platform wgpu viewport for 2D CAD and 3D design, and four supervised worker processes for 3D rendering, BIM/IFC, CAD heavy-lift, and local AI inference. The Electron main process enforces a strict security boundary between the renderer and native capabilities.

For the full technical architecture, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Workflow modes

| Mode | Purpose | Main users |
|---|---|---|
| **Home** | Project dashboard, templates, recents, asset library entry point | Everyone |
| **Design** | 3D space modeling, furniture placement, materials, lighting, cameras | Interior designers, architects |
| **Draft** | 2D CAD drawings, sheets, dimensions, schedules, title blocks | Drafters, architects |
| **BIM** | IFC import/export, classification, property editing, schedules, takeoff | BIM coordinators, small firms |
| **Render** | Native PBR rasterized previews, path-traced final renders, batch queues, walkthroughs, panoramas — all in-process Rust | Visualizers |
| **Deliver** | Proposal packs, contractor handoff, BOQ exports, IFC packs, PDF/DXF | Project leads |
| **Settings** | Hardware profile, AI model tier override, render defaults, units, KChat integration toggle | Everyone |

A Ctrl/Cmd+K **command palette** opens from any mode, fuzzy-searching every registered command and shortcut. Navigation shortcuts: `Ctrl/Cmd+1..6` for Home / Design / Draft / BIM / Render / Deliver, `Ctrl/Cmd+,` for Settings.

---

## KChat integration (optional, Phase 7)

AEC Studio ships with optional **KChat** integration that's strictly local-first:

- **Outbound** — artefacts (renders, sheets, revision packs, BOQ snapshots) can be published as inline cards to a KChat thread with one click. The user supplies the transport; nothing routes through a centralised AEC Studio service.
- **Inbound** — review and approval comments on those cards are ingested back into the project's audit trail with `ActorKind::KChat` so every decision is traceable.
- **Off by default** — when the KChat toggle in Settings is off, every publish/sync method returns `KChatDisabled` and the corresponding UI hides itself. AEC Studio remains fully functional without ever touching KChat.

The full Phase 7 component listing lives in [PHASES.md](PHASES.md) and the technical design is in [ARCHITECTURE.md](ARCHITECTURE.md#96-kchat-integration).

---

## Open-source foundations

AEC Studio learns from — and selectively interoperates with — battle-tested open-source projects:

| Project | What AEC Studio uses it for |
|---|---|
| **kennguy3n/cycles** | Reference implementation studied for the native path tracer (BVH traversal, principled BSDF, sampler) — not a runtime dependency |
| **QCAD / LibreCAD** | 2D CAD UX patterns (snaps, command line, layers, dim styles) |
| **IfcOpenShell / Bonsai / FreeCAD** | Reference implementations studied for IFC parsing, geometry, property editing patterns — not runtime dependencies |

---

## Reference repositories

| Repo | Role |
|---|---|
| [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) | Local AI inference (PrismML fork — Q1_0_g128 ternary repack, CUDA, Metal, Vulkan, AVX-512 VNNI, AVX-VNNI, AVX2, ARM NEON) |
| [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) | Reference implementation studied for the native Rust STEP parser (not a runtime dependency) |
| [kennguy3n/cycles](https://github.com/kennguy3n/cycles) | Reference implementation studied for the native Rust path tracer (not a runtime dependency) |
| [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge) | Local knowledge substrate patterns (SQLCipher, scopes, audit) |
| [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera) | Reference Electron + Rust desktop app structure |

---

## Quick start

### Prerequisites

- **Rust** 1.75+ (`rustup` recommended)
- **Node.js** 20+ and **npm** 10+
- **C toolchain** for native dependency compilation (gcc/clang on Linux/macOS, MSVC on Windows)
- **CMake** for native dependencies (wgpu native, SQLCipher, OpenSSL)


#### Linux prerequisites

On Ubuntu / Debian, install the Electron runtime libraries before launching the desktop shell:

```bash
sudo apt-get install -y libgtk-3-0 libnss3 libxss1 libasound2 libnotify4 \
  build-essential cmake pkg-config libssl-dev libudev-dev
```

Optional (used at runtime if present, for GPU detection only):

```bash
sudo apt-get install -y pciutils vulkan-tools
```

The Rust workspace probes `/proc/driver/nvidia/version`, `lspci`, and `vulkaninfo` for GPU detection; missing tools just fall back to a software profile. Rendering and BIM parsing are fully native Rust — no Blender or IfcOpenShell installation is required at runtime.

### Setup

```bash
git clone https://github.com/kennguy3n/AEC-Studio.git
cd AEC-Studio
npm install
cargo build --all-targets
npm run build:native
```

### Run tests

```bash
# Rust tests
cargo test --all

# TypeScript / React tests
npm test

# Lint
cargo clippy --all-targets --all-features -- -D warnings
npm run lint

# Type-check
npm run type-check
```

### Development

```bash
# Start Vite dev server (renderer only — Electron shell requires packaging)
npm run dev --workspace=apps/desktop
```

---

## Repository layout

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
│   ├── aec_bim/                # Native BIM/IFC: STEP reader/writer, tessellator, spatial hierarchy
│   ├── aec_render/             # Native path tracer + PBR preview + walkthrough/panorama
│   ├── aec_assets/             # Asset database, import pipeline, LOD, thumbnails
│   ├── aec_materials/          # PBR material library, texture management
│   ├── aec_ai/                 # AI command planner, tool schema, safety validator
│   ├── aec_governor/           # Resource governor, hardware profiler, scheduling
│   ├── aec_export/             # PDF, DXF, IFC, glTF, proposal pack export
│   └── aec_audit/              # Audit trail, project history
├── workers/                    # Sidecar processes
│   └── ai/                     # llama-server sidecar config
├── templates/                  # Project, room, drawing, render, BIM templates
│   ├── interior/
│   ├── architecture/
│   └── drafting/
├── assets/                     # Bundled asset packs
│   ├── furniture/
│   ├── materials/
│   └── presets/
├── packaging/                  # electron-builder configs
│   ├── linux/                  # AppImage, .deb, .snap + .desktop file
│   ├── macos/
│   └── windows/
├── docs/                       # Additional documentation
├── .github/workflows/          # CI configuration
├── LICENSE                     # AGPL-3.0
├── README.md
├── CONTRIBUTING.md
├── SECURITY.md
├── PROPOSAL.md
├── ARCHITECTURE.md
└── PROGRESS.md
```

---

## Design system

AEC Studio's UI follows the **KChat design system** (the same token set used by [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera)).

| Token | Value |
|---|---|
| **Primary accent** | `#7C3AED` (Purple/Violet) — headlines, CTA buttons, active states, links, icons |
| **Primary hover** | `#6D28D9` (darker violet) |
| **Background – page** | `#FFFFFF` (white) |
| **Background – card/surface** | `#F5F3FF` (light lavender) or `#F9FAFB` (light gray) |
| **Text – headline** | `#111827` (near-black) |
| **Text – body** | `#4B5563` (dark gray) |
| **Text – secondary** | `#6B7280` (medium gray) |
| **Font family** | `Inter` (primary), system sans-serif fallback stack: `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif` |
| **Primary button** | Solid `#7C3AED` background, white text, pill/rounded shape (`border-radius: 9999px`) |
| **Secondary button** | Outlined with `#111827` border, dark text, uppercase tracking |
| **Cards** | White `#FFFFFF` background, `border-radius: 12px`, subtle shadow `0 1px 3px rgba(0,0,0,0.1)` |
| **Overall feel** | Clean, modern, minimal — purple dominant against white/light surfaces |

---

## License

AGPL-3.0 — see [LICENSE](LICENSE).

---

## Links

- [PROPOSAL.md](PROPOSAL.md) — full product proposal
- [ARCHITECTURE.md](ARCHITECTURE.md) — technical architecture
- [PROGRESS.md](PROGRESS.md) — phased delivery tracker
- [PHASES.md](PHASES.md) — top-line phase status
- [EXTENSIONS.md](EXTENSIONS.md) — extension system: manifest schema, permissions, signatures
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution guide
- [SECURITY.md](SECURITY.md) — security policy
- [docs/LICENSE_ARCHITECTURE.md](docs/LICENSE_ARCHITECTURE.md) — AGPL boundary analysis (llama.cpp; rendering and BIM are now in-process Rust)
- [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) — local AI inference
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — reference implementation studied for the native Rust STEP parser (not a runtime dependency)
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — reference implementation studied for the native Rust path tracer (not a runtime dependency)
- [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge) — local knowledge substrate
- [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera) — reference desktop architecture
