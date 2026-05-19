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

**Status:** `NOT STARTED`

**Goal:** Prove that the planned stack — Electron + Rust + wgpu + N-API + Blender worker + IfcOpenShell worker + PrismML sidecar — works end to end at a small scale before building product features on it.

### Build

| Item | Status |
|---|---|
| License architecture decision (AGPL boundaries with Blender, IfcOpenShell, Cycles) | `NOT STARTED` |
| Rust workspace setup (`Cargo.toml`, `rustfmt.toml`, clippy config matching knowledge repo) | `NOT STARTED` |
| Electron app skeleton with React renderer | `NOT STARTED` |
| TypeScript IPC layer (typed message contract, preload bridge) | `NOT STARTED` |
| Rust N-API bridge (`aec_bridge` crate) | `NOT STARTED` |
| wgpu viewport prototype (3D + 2D camera modes) | `NOT STARTED` |
| Blender worker proof of concept (EEVEE preview round-trip) | `NOT STARTED` |
| Local project package format (`.aecstudio` manifest + SQLite + commands) | `NOT STARTED` |
| Basic asset pipeline (import → LOD → thumbnail → asset DB) | `NOT STARTED` |
| llama.cpp PrismML sidecar proof of concept (Bonsai 1.7B tool call) | `NOT STARTED` |
| DXF import / export spike | `NOT STARTED` |
| IfcOpenShell import / mesh spike | `NOT STARTED` |
| Hardware profiler prototype (CPU, RAM, GPU, accelerators) | `NOT STARTED` |
| Resource governor skeleton (policy → scheduler → UI report) | `NOT STARTED` |

### Exit criteria

- [ ] Electron + React renderer launches and routes typed IPC into the Rust core.
- [ ] wgpu viewport draws geometry from the Rust core, with 3D and 2D camera modes.
- [ ] A round-trip render through the Blender worker produces a PNG visible in the renderer.
- [ ] PrismML sidecar accepts a tool-call request and returns a grammar-constrained JSON response.
- [ ] DXF and IFC import + export work on a small sample at acceptable performance.
- [ ] License posture documented for AGPL ↔ GPL (Blender) ↔ LGPL (IfcOpenShell) ↔ Apache (Cycles) ↔ MIT (llama.cpp).

---

## Phase 2 — ArchViz / Interior Studio MVP

**Status:** `NOT STARTED`

**Goal:** A solo interior designer can take an apartment from new project to client renders without ever leaving the app.

### Build

| Item | Status |
|---|---|
| Home screen with project dashboard and templates | `NOT STARTED` |
| Design mode UI (3D viewport, toolbar, inspectors, AI panel) | `NOT STARTED` |
| Project templates: apartment, café, office, villa, retail, kitchen, bathroom, renovation | `NOT STARTED` |
| Room / wall / floor / ceiling modeling | `NOT STARTED` |
| Door / window placement with automatic wall cut | `NOT STARTED` |
| Furniture asset browser (tag-faceted, drag-to-place) | `NOT STARTED` |
| Material library (PBR, vendor packs, instance overrides) | `NOT STARTED` |
| Lighting and camera presets (warm evening, daylight, studio) | `NOT STARTED` |
| wgpu design viewport (selection halos, gizmos, snapping) | `NOT STARTED` |
| EEVEE preview via Blender worker | `NOT STARTED` |
| Cycles final render via Blender worker | `NOT STARTED` |
| Render queue (single + batch, resume on failure) | `NOT STARTED` |
| Client PDF export (cover, mood board, plan, renders, schedule) | `NOT STARTED` |
| Local AI: plan detection | `NOT STARTED` |
| Local AI: style assistant | `NOT STARTED` |
| Local AI: render doctor | `NOT STARTED` |
| Local AI: layout suggestions | `NOT STARTED` |

### Exit criteria

- [ ] An interior designer can model a one-room apartment, place furniture, render four cameras, and export a PDF concept pack in a single session.
- [ ] Plan-detection AI surfaces the proposed walls as a previewable diff before commit.
- [ ] All AI actions are recorded in the audit trail.
- [ ] Cycles renders resume on failure.

---

## Phase 3 — 2D CAD module

**Status:** `NOT STARTED`

**Goal:** A drafter can produce construction documentation in pure 2D, with or without using the 3D module, and roundtrip DXF cleanly.

### Build

| Item | Status |
|---|---|
| Draft mode UI (canvas, toolbar, layers, sheet manager) | `NOT STARTED` |
| Native 2D CAD canvas (Rust / wgpu, orthographic) | `NOT STARTED` |
| Drawing primitives (line, polyline, arc, circle, ellipse, spline, hatch, text) | `NOT STARTED` |
| Editing tools (move, copy, rotate, scale, mirror, offset, trim, extend, fillet, chamfer) | `NOT STARTED` |
| Precision tools (grid, ortho, polar, snaps, tracking, parametric constraints) | `NOT STARTED` |
| Layer system (state manager, freeze/thaw, color, lineweight, linetype) | `NOT STARTED` |
| Block system (library, dynamic blocks, attributes) | `NOT STARTED` |
| Dimension tools (linear, angular, radial, baseline, continue) | `NOT STARTED` |
| Sheet layout and title blocks | `NOT STARTED` |
| DXF import / export (roundtripped layers, blocks, dim styles) | `NOT STARTED` |
| DWG converter adapter (out-of-process, opt-in) | `NOT STARTED` |
| PDF / SVG export (deterministic, sheet sets) | `NOT STARTED` |
| Command line parser (`L`, `O`, `CO`, `MO`, `TRIM`, `EX`, `F`) | `NOT STARTED` |
| Local AI: CAD cleanup (gap close, duplicate removal, layer normalize) | `NOT STARTED` |
| Local AI: plan-to-wall conversion | `NOT STARTED` |

### Exit criteria

- [ ] A drafter can deliver a 12-sheet set using keyboard-driven commands.
- [ ] DXF roundtrips lossless on layer, block, dim style, and text style.
- [ ] DWG export is available but explicitly opt-in.
- [ ] AI CAD cleanup actions are previewed as diffs before commit.

---

## Phase 4 — BIM Lite / IFC

**Status:** `NOT STARTED`

**Goal:** A small studio can import IFC, classify, edit properties, generate schedules and BOQ-lite, and re-export with GUID preservation.

### Build

| Item | Status |
|---|---|
| BIM mode UI (spatial tree, property editor, schedule view, validator panel) | `NOT STARTED` |
| IFC import via IfcOpenShell (IFC2x3 / IFC4 / IFC4x3) | `NOT STARTED` |
| IFC export with GUID preservation | `NOT STARTED` |
| Spatial hierarchy (project / site / building / level / space) | `NOT STARTED` |
| BIM element classification (IfcWall, IfcSlab, IfcDoor, IfcWindow, IfcFurniture, ...) | `NOT STARTED` |
| Object property editor (Pset_*, Qto_*, custom psets, type/instance) | `NOT STARTED` |
| Room schedule | `NOT STARTED` |
| Door / window schedule | `NOT STARTED` |
| Material schedule | `NOT STARTED` |
| Quantity takeoff / BOQ-lite (areas, counts per discipline) | `NOT STARTED` |
| Validation engine (dangling refs, missing classes, unclosed spaces, duplicate GUIDs) | `NOT STARTED` |
| IFC model diff (element + property level) | `NOT STARTED` |
| Local AI: classification and property fill | `NOT STARTED` |
| Drawing generation from BIM model | `NOT STARTED` |

### Exit criteria

- [ ] A 40 MB IFC opens in under 15 s on a mid-tier laptop.
- [ ] IFC export validates strict mode and roundtrips with full GUID match on unmodified elements.
- [ ] BOQ-lite XLSX accounts for at least 95 % of materials by area / count.
- [ ] AI classification confidence threshold is configurable.

---

## Phase 5 — Render pipeline hardening

**Status:** `NOT STARTED`

**Goal:** Renders are reliable, reproducible, and fast enough to be part of the daily delivery workflow.

### Build

| Item | Status |
|---|---|
| Render mode UI (queue, preview, presets, doctor) | `NOT STARTED` |
| Saved camera management (focal length, exposure, WB, DoF) | `NOT STARTED` |
| Render presets system (Quick, Standard, High, Studio, EEVEE Preview, Walkthrough, Panorama) | `NOT STARTED` |
| Lighting presets (sun + sky model, IES profiles, mood presets) | `NOT STARTED` |
| Material check / doctor (missing textures, non-PBR, channels swapped) | `NOT STARTED` |
| Batch render queue (multi-camera, multi-preset) | `NOT STARTED` |
| Render history and before / after compare | `NOT STARTED` |
| Panorama render (Cycles equirectangular) | `NOT STARTED` |
| Walkthrough render (Cycles + camera path) | `NOT STARTED` |
| Render resume on failure (frame-level for walkthrough) | `NOT STARTED` |

### Exit criteria

- [ ] A user can queue 8 renders overnight on a mid-tier PC and resume any that crashed.
- [ ] Render history surfaces a before / after compare across revisions.
- [ ] Walkthrough renders resume from the last completed frame.
- [ ] EEVEE preview latency stays under 250 ms on a mid-tier laptop with a typical interior scene.

---

## Phase 6 — Deliver and export

**Status:** `NOT STARTED`

**Goal:** A studio lead can ship a complete delivery package from one project — client, contractor, BIM, and revision-tracked.

### Build

| Item | Status |
|---|---|
| Deliver mode UI (pack composer, export targets, revision manager) | `NOT STARTED` |
| Client concept pack (cover, mood board, plan, renders, schedule) | `NOT STARTED` |
| Interior package export (PDF + image archive + material schedule) | `NOT STARTED` |
| Contractor handoff pack (sheets, schedules, IFC, BOQ-lite) | `NOT STARTED` |
| BIM Lite export pack (IFC + sheets + validation report) | `NOT STARTED` |
| Revision system (tagged snapshots, audit-linked) | `NOT STARTED` |
| Version comparison (geometry, sheets, schedules) | `NOT STARTED` |
| Before / after generation (renders, plans) | `NOT STARTED` |
| Proposal PDF generation (AI-assisted cover paragraph) | `NOT STARTED` |
| Material schedule export (XLSX) | `NOT STARTED` |
| BOQ export (XLSX, configurable per region) | `NOT STARTED` |

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
| KChat artifact card publishing (render, sheet, revision pack, BOQ snapshot) | `NOT STARTED` |
| Review / approval cards (inline comments → audit-trail entries) | `NOT STARTED` |
| Revision comments sync (one-way: KChat → audit trail) | `NOT STARTED` |
| Team asset packs (publish + subscribe via the user's existing transport) | `NOT STARTED` |
| Local-first sync (no centralized store; uses the user's own transport) | `NOT STARTED` |

### Exit criteria

- [ ] Users can publish a render or sheet pack to a KChat thread in one click.
- [ ] KChat comments appear as audit-trail entries with thread context.
- [ ] AEC Studio remains fully usable with KChat integration disabled.

---

## MVP feature set summary

| Category | Details |
|---|---|
| **Platforms** | macOS (Intel + Apple Silicon), Windows x64 |
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
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution guide
- [SECURITY.md](SECURITY.md) — security policy
- [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) — local AI inference
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — BIM/IFC engine
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — path-traced renderer

---

## Changelog

### 2026-05-19

- Phase 0 completed: Repository, AGPL-3.0 license, and the full documentation suite (README, PROPOSAL, ARCHITECTURE, PROGRESS, CONTRIBUTING, SECURITY).
