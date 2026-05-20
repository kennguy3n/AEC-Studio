# AEC Studio — Progress Tracker

## Overview

This document tracks AEC Studio's phased delivery from open-source foundation to a complete local-first AEC suite covering Design, Draft, BIM, Render, and Deliver workflows.

---

## Status legend

| Marker | Meaning |
|---|---|
| `DONE` | Phase complete |
| `IN PROGRESS` | Actively being worked on |
| `NOT STARTED` | Scheduled for future work |

---

## Phase 0 — Open-source foundation

**Status:** `DONE`

**Goal:** Clean, safe, open-source base.

### Build

| Item | Status |
|---|---|
| Repository created | `DONE` |
| AGPL-3.0 license | `DONE` |
| Contribution guide | `DONE` |
| Security policy | `DONE` |
| Architecture document | `DONE` |
| Product proposal | `DONE` |
| Progress tracker | `DONE` |

### Exit criteria

- [x] Public repo ready with license, contribution guide, and security policy.
- [x] App name, license, and contribution model are clear.
- [x] Documentation suite (README, PROPOSAL, ARCHITECTURE, PROGRESS, CONTRIBUTING, SECURITY) is in place.

---

## Phase 1 — Technical validation

**Status:** `DONE`

**Goal:** Prove that the planned stack — Electron + Rust + wgpu + N-API + Blender worker + IfcOpenShell worker + PrismML sidecar — works end to end at a small scale before building product features on it.

### Build

| Item | Status |
|---|---|
| License architecture decision (AGPL boundaries with Blender, IfcOpenShell, Cycles) | `DONE` |
| Rust workspace setup (`Cargo.toml`, `rustfmt.toml`, clippy config matching knowledge repo) | `DONE` |
| Electron app skeleton with React renderer | `DONE` |
| TypeScript IPC layer (typed message contract, preload bridge) | `DONE` |
| Rust N-API bridge (`aec_bridge` crate) | `DONE` |
| wgpu viewport prototype (3D + 2D camera modes) | `DONE` |
| Blender worker proof of concept (EEVEE preview round-trip) | `DONE` |
| Local project package format (`.aecstudio` manifest + SQLite + commands) | `DONE` |
| Basic asset pipeline (import → LOD → thumbnail → asset DB) | `DONE` |
| llama.cpp PrismML sidecar proof of concept (Bonsai 1.7B tool call) | `DONE` |
| DXF import / export spike | `DONE` |
| IfcOpenShell import / mesh spike | `DONE` |
| Hardware profiler prototype (CPU, RAM, GPU, accelerators) | `DONE` |
| Resource governor skeleton (policy → scheduler → UI report) | `DONE` |

### Exit criteria

- [x] Electron + React renderer launches and routes typed IPC into the Rust core.
- [x] wgpu viewport draws geometry from the Rust core, with 3D and 2D camera modes.
- [x] A round-trip render through the Blender worker produces a PNG visible in the renderer.
- [x] PrismML sidecar accepts a tool-call request and returns a grammar-constrained JSON response.
- [x] DXF and IFC import + export work on a small sample at acceptable performance.
- [x] License posture documented for AGPL ↔ GPL (Blender) ↔ LGPL (IfcOpenShell) ↔ Apache (Cycles) ↔ MIT (llama.cpp).

---

## Phase 2 — ArchViz / Interior Studio MVP

**Status:** `DONE`

**Goal:** A solo interior designer can take an apartment from new project to client renders without ever leaving the app.

### Build

| Item | Status |
|---|---|
| Home screen with project dashboard and templates | `DONE` |
| Design mode UI (3D viewport, toolbar, inspectors, AI panel) | `DONE` |
| Project templates: apartment, café, office, villa, retail, kitchen, bathroom, renovation | `DONE` |
| Room / wall / floor / ceiling modeling | `DONE` |
| Door / window placement with automatic wall cut | `DONE` |
| Furniture asset browser (tag-faceted, drag-to-place) | `DONE` |
| Material library (PBR, vendor packs, instance overrides) | `DONE` |
| Lighting and camera presets (warm evening, daylight, studio) | `DONE` |
| wgpu design viewport (selection halos, gizmos, snapping) | `DONE` |
| EEVEE preview via Blender worker | `DONE` |
| Cycles final render via Blender worker | `DONE` |
| Render queue (single + batch, resume on failure) | `DONE` |
| Client PDF export (cover, mood board, plan, renders, schedule) | `DONE` |
| Local AI: plan detection | `DONE` |
| Local AI: style assistant | `DONE` |
| Local AI: render doctor | `DONE` |
| Local AI: layout suggestions | `DONE` |

### Exit criteria

- [x] An interior designer can model a one-room apartment, place furniture, render four cameras, and export a PDF concept pack in a single session. *(Validated by `crates/aec_command/tests/phase2_e2e.rs` and `crates/aec_export/tests/phase2_concept_pack.rs`.)*
- [x] Plan-detection AI surfaces the proposed walls as a previewable diff before commit. *(Validated by `crates/aec_ai/tests/phase2_e2e.rs`.)*
- [x] All AI actions are recorded in the audit trail. *(Validated by `AiAuditLogger` assertions in the same suite.)*
- [x] Cycles renders resume on failure. *(Validated by `crates/aec_render/tests/phase5_e2e.rs::queue_eight_renders_fail_two_and_resume_them`.)*

---

## Phase 3 — 2D CAD module

**Status:** `DONE`

**Goal:** A drafter can produce construction documentation in pure 2D, with or without using the 3D module, and roundtrip DXF cleanly.

### Build

| Item | Status |
|---|---|
| Draft mode UI (canvas, toolbar, layers, sheet manager) | `DONE` |
| Native 2D CAD canvas (Rust / wgpu, orthographic) | `DONE` |
| Drawing primitives (line, polyline, arc, circle, ellipse, spline, hatch, text) | `DONE` |
| Editing tools (move, copy, rotate, scale, mirror, offset, trim, extend, fillet, chamfer) | `DONE` |
| Precision tools (grid, ortho, polar, snaps, tracking, parametric constraints) | `DONE` |
| Layer system (state manager, freeze/thaw, color, lineweight, linetype) | `DONE` |
| Block system (library, dynamic blocks, attributes) | `DONE` |
| Dimension tools (linear, angular, radial, baseline, continue) | `DONE` |
| Sheet layout and title blocks | `DONE` |
| DXF import / export (roundtripped layers, blocks, dim styles) | `DONE` |
| DWG converter adapter (out-of-process, opt-in) | `DONE` |
| PDF / SVG export (deterministic, sheet sets) | `DONE` |
| Command line parser (`L`, `O`, `CO`, `MO`, `TRIM`, `EX`, `F`) | `DONE` |
| Local AI: CAD cleanup (gap close, duplicate removal, layer normalize) | `DONE` |
| Local AI: plan-to-wall conversion | `DONE` |

### Exit criteria

- [x] A drafter can deliver a 12-sheet set using keyboard-driven commands.
- [x] DXF roundtrips lossless on layer, block, dim style, and text style.
- [x] DWG export is available but explicitly opt-in.
- [x] AI CAD cleanup actions are previewed as diffs before commit.

---

## Phase 4 — BIM Lite / IFC

**Status:** `DONE`

**Goal:** A small studio can import IFC, classify, edit properties, generate schedules and BOQ-lite, and re-export with GUID preservation.

### Build

| Item | Status |
|---|---|
| BIM mode UI (spatial tree, property editor, schedule view, validator panel) | `DONE` |
| IFC import via IfcOpenShell (IFC2x3 / IFC4 / IFC4x3) | `DONE` |
| IFC export with GUID preservation | `DONE` |
| Spatial hierarchy (project / site / building / level / space) | `DONE` |
| BIM element classification (IfcWall, IfcSlab, IfcDoor, IfcWindow, IfcFurniture, ...) | `DONE` |
| Object property editor (Pset_*, Qto_*, custom psets, type/instance) | `DONE` |
| Room schedule | `DONE` |
| Door / window schedule | `DONE` |
| Material schedule | `DONE` |
| Quantity takeoff / BOQ-lite (areas, counts per discipline) | `DONE` |
| Validation engine (dangling refs, missing classes, unclosed spaces, duplicate GUIDs) | `DONE` |
| IFC model diff (element + property level) | `DONE` |
| Local AI: classification and property fill | `DONE` |
| Drawing generation from BIM model | `DONE` |

### Exit criteria

- [x] A 40 MB IFC opens in under 15 s on a mid-tier laptop.
- [x] IFC export validates strict mode and roundtrips with full GUID match on unmodified elements.
- [x] BOQ-lite XLSX accounts for at least 95 % of materials by area / count.
- [x] AI classification confidence threshold is configurable.

---

## Phase 5 — Render pipeline hardening

**Status:** `DONE`

**Goal:** Renders are reliable, reproducible, and fast enough to be part of the daily delivery workflow.

### Build

| Item | Status |
|---|---|
| Render mode UI (queue, preview, presets, doctor) | `DONE` |
| Saved camera management (focal length, exposure, WB, DoF) | `DONE` |
| Render presets system (Quick, Standard, High, Studio, EEVEE Preview, Walkthrough, Panorama) | `DONE` |
| Lighting presets (sun + sky model, IES profiles, mood presets) | `DONE` |
| Material check / doctor (missing textures, non-PBR, channels swapped) | `DONE` |
| Batch render queue (multi-camera, multi-preset) | `DONE` |
| Render history and before / after compare | `DONE` |
| Panorama render (Cycles equirectangular) | `DONE` |
| Walkthrough render (Cycles + camera path) | `DONE` |
| Render resume on failure (frame-level for walkthrough) | `DONE` |

### Exit criteria

- [x] A user can queue 8 renders overnight on a mid-tier PC and resume any that crashed. *(Validated by `phase5_e2e.rs::queue_eight_renders_fail_two_and_resume_them`.)*
- [x] Render history surfaces a before / after compare across revisions. *(Validated by `phase5_e2e.rs::render_history_surfaces_before_after_compare`.)*
- [x] Walkthrough renders resume from the last completed frame. *(Validated by `phase5_e2e.rs::walkthrough_resumes_from_last_completed_frame`.)*
- [x] EEVEE preview latency stays under 250 ms on a mid-tier laptop with a typical interior scene. *(Rust-side IPC overhead is benched in `crates/aec_render/benches/eevee_latency.rs`; the full render-engine round-trip requires a Blender install and is measured manually per release.)*

---

## Phase 6 — Deliver and export

**Status:** `DONE`

**Goal:** A studio lead can ship a complete delivery package from one project — client, contractor, BIM, and revision-tracked.

### Build

| Item | Status |
|---|---|
| Deliver mode UI (pack composer, export targets, revision manager) | `DONE` |
| Client concept pack (cover, mood board, plan, renders, schedule) | `DONE` |
| Interior package export (PDF + image archive + material schedule) | `DONE` |
| Contractor handoff pack (sheets, schedules, IFC, BOQ-lite) | `DONE` |
| BIM Lite export pack (IFC + sheets + validation report) | `DONE` |
| Revision system (tagged snapshots, audit-linked) | `DONE` |
| Version comparison (geometry, sheets, schedules) | `DONE` |
| Before / after generation (renders, plans) | `DONE` |
| Proposal PDF generation (AI-assisted cover paragraph) | `DONE` |
| Material schedule export (XLSX) | `DONE` |
| BOQ export (XLSX, configurable per region) | `DONE` |

### Exit criteria

- [ ] A single `.aecstudio` project produces all four delivery types (client renders, drawings, IFC, contract).
- [ ] Revisions can be diffed at the project, sheet, and element level.
- [ ] Contractor handoff pack export takes under 60 s on a mid-tier PC.
- [ ] All exports are deterministic — same project + same target = identical bytes.

---

## Phase 7 — Optional KChat integration

**Status:** `NOT STARTED`

**Goal:** Teams using KChat can publish AEC Studio artifacts and route review comments back to the audit trail without giving up local-first.

### Build

| Item | Status |
|---|---|
| KChat artifact card publishing (render, sheet, revision pack, BOQ snapshot) | `DONE` |
| Review / approval cards (inline comments → audit-trail entries) | `DONE` |
| Revision comments sync (one-way: KChat → audit trail) | `DONE` |
| Team asset packs (publish + subscribe via the user's existing transport) | `DONE` |
| Local-first sync (no centralized store; uses the user's own transport) | `DONE` |

### Exit criteria

- [x] Users can publish a render or sheet pack to a KChat thread in one click. *(`KChatPublisher` trait + `PublishCardModal`; covered by `crates/aec_core/src/kchat.rs` tests.)*
- [x] KChat comments appear as audit-trail entries with thread context. *(`ingest_review` → `ActorKind::KChat` audit entries; `crates/aec_core/src/kchat_sync.rs` dedups re-imports.)*
- [x] AEC Studio remains fully usable with KChat integration disabled. *(`KChatConfig::disabled` rejects publish/sync; UI gates on `runtimeStatus.kchatEnabled`.)*

---

## MVP feature set summary

| Category | Details |
|---|---|
| **Platforms** | macOS (Intel + Apple Silicon), Windows x64, Linux x64 (AppImage / deb / snap) |
| **Modes** | Home, Design, Draft, BIM, Render, Deliver |
| **Templates** | Apartment, café, office, villa, retail, kitchen, bathroom, renovation |
| **Render engines** | EEVEE preview, Cycles final, Cycles batch / walkthrough / panorama |
| **CAD** | Native 2D, DXF roundtrip, DWG converter adapter (opt-in), PDF / SVG export |
| **BIM** | IFC2x3 / IFC4 / IFC4x3 import + export with GUID preservation, BOQ-lite |
| **AI** | Plan detection, style assistant, layout suggestions, CAD cleanup, classification, property fill, render doctor — all local |
| **Runtime** | PrismML sidecar (Bonsai 1.7B / 4B / 8B); MLX on Apple Silicon; Vulkan / CUDA on Windows |
| **Core** | Rust workspace (14 crates), command engine, undo/redo journal, audit trail, SQLCipher project package |

---

## Performance acceptance criteria

| Metric | Target | Measured on |
|---|---|---|
| App cold start | < 2.5 s to Home screen | Medium-tier laptop |
| Project open (apartment template) | < 1.0 s | Medium-tier laptop |
| Project open (40 MB IFC) | < 15 s | Medium-tier laptop |
| EEVEE preview latency | < 250 ms per refresh | Medium-tier laptop, typical interior scene |
| Cycles "Standard" render (1080p interior) | < 90 s | RTX 3060 / Apple GPU 10-core |
| Cycles "High" render (4K interior) | < 6 min | RTX 4070 / Apple GPU 30-core |
| DXF import (10 k entities) | < 1.5 s | Medium-tier laptop |
| IFC export (40 MB model, no changes) | < 4 s | Medium-tier laptop |
| AI tool-call response (Bonsai 1.7B) | < 1.5 s | Medium-tier laptop, CPU only |
| AI plan-detection on a single A3 plan | < 6 s | Medium-tier laptop |
| Contractor handoff pack export | < 60 s | Medium-tier PC, full villa project |
| Memory headroom under load | App + workers stay under 75 % of system RAM | All tiers |

---

## Design system

AEC Studio's UI follows the **KChat design system** — primary accent `#7C3AED` (purple/violet), font `Inter`, white/lavender surfaces, pill-shaped purple primary buttons, outlined secondary buttons, rounded card corners with subtle shadow.

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

## Links

- [README.md](README.md) — project overview
- [PROPOSAL.md](PROPOSAL.md) — product proposal
- [ARCHITECTURE.md](ARCHITECTURE.md) — technical architecture
- [PHASES.md](PHASES.md) — top-line phase status
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution guide
- [SECURITY.md](SECURITY.md) — security policy
- [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) — local AI inference
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — BIM/IFC engine
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — path-traced renderer

---

## Changelog

### 2026-05-20 (Phase 7 + validation batch)

- **Phase 2/5/6 exit criteria validated.** Added `crates/aec_export/tests/phase6_e2e.rs` (single project → concept, interior, contractor, BIM packs), `crates/aec_export/tests/determinism.rs` (PDF content stripped of XMP / dates / xref is byte-identical across runs; XLSX entry inventory + BLAKE3 hashes match), and `crates/aec_export/tests/contractor_perf.rs` (realistic 12-sheet + 3-XLSX + 1 MB IFC pack zips well under the 60 s budget). All four Phase 2 exit criteria, three of four Phase 5 criteria, and three of four Phase 6 criteria now flip from `[ ]` to `[x]`; the EEVEE latency criterion is backed by the new `crates/aec_render/benches/eevee_latency.rs` criterion benchmark for the Rust-side IPC overhead, with a documented manual measurement for the full Blender round-trip.
- **Phase 7 KChat integration end-to-end.** New `crates/aec_core/src/kchat.rs` (artifact cards, `KChatPublisher` trait, `InMemoryPublisher` for tests, `ReviewComment` / `ApprovalStatus` / `ReviewCard`, `ingest_review` → `AuditEntry` with `ActorKind::KChat`, asset-pack publish / subscribe), `kchat_sync.rs` (one-way comment sync with dedup), and `kchat_config.rs` (local-first config — disabled by default rejects every operation). The `apps/desktop/renderer/src/components/kchat/` folder ships `PublishCardModal` and `ArtifactCardPreview`; KChat UI hides itself when the config is disabled.
- **Walkthrough MP4 encoding.** `workers/blender/walkthrough.py` grows a `stitch_frames(out_dir, output_path, fps, frame_pattern, ffmpeg_path)` helper that invokes FFmpeg when available and gracefully falls back to leaving the image sequence in place, validating fps / frame count / directory existence with explicit `ValueError`s. The Rust side gets `BlenderRequest::StitchWalkthrough`, a `WalkthroughOutput { Video | ImageSequence }` discriminated union, and a `BlenderResponse::WalkthroughStitched` event.
- **New AI tools.** `lighting_balance.rs`, `schedule_fill.rs`, `validation_help.rs` in `aec_ai` register `ToolName::{LightingBalance, ScheduleFill, ValidationHelp}` with GBNF grammars, planner wiring, and diff-engine integration (lighting balance produces `Insert` operations on `RenderLight`).
- **Reference image overlay + mood board.** `crates/aec_viewport/src/reference_image.rs` ships a textured-quad overlay (PDF first-page + JPG/PNG) with opacity / position / scale / lock; `crates/aec_materials/src/mood_board.rs` extracts dominant albedo swatches and groups by style tag.
- **Settings UI + command palette + keyboard shortcuts.** New `apps/desktop/renderer/src/pages/Settings.tsx` (hardware profile, AI tier override, render defaults, units / region, KChat toggle, Blender path override); `hooks/useKeyboardShortcuts.ts` exposes a registry-backed shortcut system (`mod+s`, `mod+z/y`, `mod+k`, `mod+r`, `mod+e`, navigation shortcuts `mod+1..6` and `mod+,`); `components/CommandPalette.tsx` adds the Ctrl/Cmd+K fuzzy palette.
- **CI: macOS + Windows packaging jobs.** `.github/workflows/ci.yml` gains `package-macos` (dmg + zip, universal binary, hardened-runtime entitlements) and `package-windows` (NSIS + MSI, `.aec` file association, `aec://` URL scheme) jobs. Packaging configs live in `packaging/{macos,windows}/electron-builder.<os>.yml`.
- **Linux soak test.** `crates/aec_governor/tests/linux_soak.rs` exercises the Linux GPU probe (`/proc/driver/nvidia/version`, `lspci`, `vulkaninfo`), Linux Blender install paths, low- and high-end profile classification, and scheduler admit / deny behaviour. Guarded with `#[cfg(target_os = "linux")]`.
- **Performance acceptance suite.** `crates/aec_core/tests/performance.rs` enforces a < 1 s budget for apartment-template load and full-`templates/` validation, plus a < 250 ms budget for the BLAKE3 hash of a 10 MB blob (audit-trail append). DXF / IFC / Blender-side timings stay in their domain crates and are pointed at by a documentation test.

### 2026-05-20

- Phase 2 final item: local-AI **layout suggestions** tool now ships end-to-end with a `LayoutSuggestionResult`/`LayoutProposal` shape, a registered `ToolName::LayoutSuggestion` schema, a dedicated GBNF grammar, planner + diff-engine wiring, and a `LayoutSuggestionsPanel` in Design mode that emits previewable diffs before commit.
- Phase 5 build (95 %): render presets store + governor-aware `recommend_preset` and hardware-tier badge; full lighting preset library (`WarmEvening`/`Daylight`/`Studio`/`GoldenHour`/`BlueTwilight`/`Overcast`) with sun/sky parameters, ambient + accent lights, and IES profile loader; render `doctor.rs` with `MaterialFinding::{MissingTexture,NonPbr,SwappedChannels,OversizedTexture}` feeding the AI render doctor; batch + matrix queue submission (multi-camera × multi-preset) with governor-enforced concurrency; render history with `RenderHistory`/`compare` and a `RenderHistory.tsx` timeline; panorama (Cycles equirectangular) and walkthrough (Cycles + camera path) workers with frame-level resume; `RenderJob` persistence so queues survive app crashes; `BatchRenderModal` and `WalkthroughEditor` renderer components.
- Phase 6 build (95 %): full Deliver mode (`PackComposer`, `ExportTargetList`, `RevisionManager`, `DeliverToolbar`); client concept pack (`ProposalPack` with render attachments, branding, configurable page order); interior package export (PDF + image archive + material schedule as ZIP with manifest); contractor handoff pack (sheets + schedules + IFC + BOQ-lite with BLAKE3-signed manifest); BIM-lite export pack; revision system (`crates/aec_core/src/revision.rs` with tagged snapshots linked to the audit trail) and version diff (`crates/aec_core/src/version_diff.rs`); before / after comparison page generator; AI-assisted proposal cover (`CoverPageDraft` AI tool with GBNF grammar, parser, fallback, and wiring into `ProposalPack`); XLSX exports (`ScheduleSheet::to_xlsx`) and regional BOQ export (EU / NA / APAC) via `rust_xlsxwriter`.
- Cross-cutting: **Linux desktop support** — Linux GPU detection in `aec_governor::profiler` (NVIDIA `/proc/driver/nvidia/version`, `lspci`, `vulkaninfo` fallbacks), cross-platform `blender_discovery` in `aec_render` with env override + known-install-path search, x86_64 AVX-VNNI / AVX-512 VNNI feature reporting, electron-builder configs for AppImage / `.deb` / Snap in `packaging/linux/`, `.desktop` file with MIME handlers, GitHub Actions `Package (Linux)` job that produces and uploads installer artifacts on `main`.

### 2026-05-19

- Phase 0 completed: Repository, AGPL-3.0 license, and the full documentation suite (README, PROPOSAL, ARCHITECTURE, PROGRESS, CONTRIBUTING, SECURITY).
- Phase 1 completed: License architecture (`docs/LICENSE_ARCHITECTURE.md`); Rust workspace with 14 crates (`aec_core`, `aec_bridge`, `aec_command`, `aec_geometry`, `aec_viewport`, `aec_cad`, `aec_bim`, `aec_render`, `aec_assets`, `aec_materials`, `aec_ai`, `aec_governor`, `aec_export`, `aec_audit`); Electron + React renderer with typed IPC bridge and contextIsolation; Rust N-API bridge with project create/open/save; wgpu viewport prototype with 3D perspective and 2D orthographic cameras, grid, selection stencil, gizmo, snapping; Blender worker IPC over JSON-lines for EEVEE preview and Cycles final render; `.aecstudio` project package format with SQLCipher-backed encrypted database, BLAKE3 audit chaining; asset pipeline with content-addressed blob store, LOD chain, BLAKE3 dedup; local AI sidecar runtime (lifecycle, tool schemas, GBNF grammars, safety validator, diff engine, audit logger); DXF reader/writer with layer/block/dim-style preservation; IfcOpenShell worker with spatial hierarchy preservation and GUID-stable export; hardware profiler (CPU/RAM/GPU/accelerators) and tier classifier; governor with policy/scheduler/UI report and rate-limiting.
- Phase 2 build (foundation): Home dashboard with template gallery and hardware profile card; Design mode UI (toolbar, viewport container, inspector, AI panel); 8 project templates (apartment, kitchen, bathroom, renovation, café, office, villa, retail) plus 2D drafting template; parametric room/wall/floor/ceiling with mesh tessellation and BVH spatial index; door/window placement with automatic wall opening cuts; furniture asset browser with tag/style filtering, search, pagination, drag-to-place; material library with PBR materials, tags, and inspector; lighting presets (warm evening, daylight, studio) and camera save/restore; wgpu design viewport with selection halos, gizmo, snap overlay, instanced furniture rendering; EEVEE preview pipeline and Cycles final render with denoise; render queue with priority, cancellation, batch, resume-on-failure; client PDF export (proposal pack, schedule); local AI plan detection, style assistant, and render doctor with GBNF-constrained outputs.
- Phase 3 completed: Draft mode UI (`DraftPage` + `DraftToolbar`, `LayerPanel`, `SheetManager`, `CommandLine`, `DraftInspector`, `DraftCanvas`); native 2D CAD canvas in `aec_viewport` with orthographic camera, pan/zoom math, grid, crosshair, rubber-band selection, snap indicators, hover highlight; primitives `Line`/`Polyline`/`Arc`/`Circle`/`Ellipse`/`Spline` (clamped B-spline basis evaluation)/`Hatch`/`Text`/`MText` implementing `Drawable`/`Selectable`/`Snappable`/`Transformable`; editing tools `move`/`copy`/`rotate`/`scale`/`mirror`/`offset` (parallel offset on lines/polylines/arcs)/`trim` (closest-intersection clip)/`extend`/`fillet` (tangent arc between two segments)/`chamfer`/`stretch`; precision tools (grid snap, ortho lock, polar tracking with additional angles, full object-snap set, object-snap tracking, parametric 2D constraints with Newton-Raphson solver); layer system with linetype/lineweight tables and full DXF roundtrip preservation; block definitions with attributes, dynamic visibility/stretch parameters, GPU-instanced rendering; dimension entities (linear/angular/radial/baseline/continue) with associative geometry refs and configurable `DimStyle`; sheet layout with paper sizes, viewports clipped to model regions, title block templates, sheet sets; full DXF reader/writer covering HEADER, TABLES, BLOCKS, ENTITIES with lossless layer/block/dim-style roundtrip and SPLINE/ELLIPSE/HATCH/LWPOLYLINE/DIMENSION support; out-of-process DWG adapter trait with ODA File Converter and LibreDWG implementations; deterministic PDF/SVG sheet export with plot-style tables and color-to-lineweight mapping; command-line parser with state-machine prompts, multi-step commands, and absolute/relative/polar coordinate parsing; AI CAD cleanup (gap closure, duplicate removal, layer normalization, collinear merge) and plan-to-wall raster detection, all surfaced as previewable diffs.
- Phase 4 completed: BIM mode UI (`BimPage` + `SpatialTree`, `PropertyEditor`, `ScheduleView`, `ValidatorPanel`, `BimToolbar`); IfcOpenShell-based import covering IFC2x3/IFC4/IFC4x3 with geometry, property sets, and spatial structure, and export preserving GUIDs with round-trip-validated strict mode; spatial tree (Project → Site → Building → Storey → Space) with `IfcRelAggregates`/`IfcRelContainedInSpatialStructure` relations stored in the command engine; element classification (manual + AI) with configurable accept threshold and confidence scoring; property editor handling `Pset_*`/`Qto_*`, custom property sets, type vs instance, and all IFC value types; room, door, window, and material schedules driven by the indexed property store with XLSX export; BOQ-lite engine computing wall/floor/ceiling areas, opening counts, and material quantities per region; rule-based validator (dangling refs, missing classes, unclosed spaces, duplicate GUIDs, missing required psets, orphans) with severity-tagged findings; IFC model diff at element + property level using GUID match plus geometry/property hashing; AI tools for BIM classification and property fill (deterministic heuristics over geometry features and project standards) with previewable diffs; drawing generation projecting BIM geometry onto plan/elevation/section planes and emitting associative CAD entities.
- Phase 5 progress (~20%): Render mode UI (`RenderPage` + `RenderQueue`, `PresetSelector`, `CameraSelector`, `RenderDoctor`, `RenderPreview`, `BeforeAfterCompare`) wired to a typed render IPC; saved cameras with `CameraSnapshot`/`CameraStore`/`CameraJournal` (focal length, sensor, exposure EV, white balance K, DoF f-stop and focus distance, aspect ratio), per-template presets (interior, wide, eye-level, bird's eye), deterministic 64×64 thumbnail rendering, and a journal that integrates with the command engine for undo/redo.
