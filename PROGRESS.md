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
- [x] Preview latency stays under 250 ms on a mid-tier laptop with a typical interior scene. *(Measured end-to-end against the native PBR rasterizer + path tracer via `crates/aec_render/benches/native_render.rs`; the in-process pipeline has no IPC or external-binary cold-start overhead to amortise.)*

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

- [x] A single `.aecstudio` project produces all four delivery types (client renders, drawings, IFC, contract). *(Validated by `crates/aec_export/tests/phase6_e2e.rs::single_project_produces_concept_interior_contractor_bim_packs`.)*
- [x] Revisions can be diffed at the project, sheet, and element level. *(Validated by `crates/aec_core/src/version_diff.rs` unit tests covering geometry / sheet / schedule-row diff categories.)*
- [x] Contractor handoff pack export takes under 60 s on a mid-tier PC. *(Validated by `crates/aec_export/tests/contractor_perf.rs::contractor_pack_zips_realistic_payload_under_60s`.)*
- [x] All exports are deterministic — same project + same target = identical bytes. *(Validated by `crates/aec_export/tests/determinism.rs` — PDFs after metadata-strip, XLSX entry inventory, ZIP manifest hashes.)*

---

## Phase 9 — Native render & BIM engine

**Status:** `DONE` (PR1–PR4 + PR5 + PR-A through PR-P merged; #9 #10 #11 #12 #13 #14–#38 #PR-P)

**Goal:** Eliminate every external runtime dependency for rendering and IFC handling. Replace the Blender worker (EEVEE/Cycles via subprocess) and IfcOpenShell worker (Python subprocess) with native Rust implementations in-process, while preserving every user-facing capability (PBR preview, path-traced final, walkthrough, panorama, IFC2x3 / IFC4 / IFC4x3 import/export with GUID + Pset round-trip).

### Build

| Item | Status |
|---|---|
| **PR1**: BVH (SAH), ray-tri intersection, CPU path tracer, principled BSDF, light sampling + MIS | `DONE` (#9 merged) |
| **PR2**: wgpu compute path tracer, bilateral denoiser, tile scheduler | `DONE` (#10 merged) |
| **PR3**: PBR rasterization preview, Hosek-Wilkie sky shader, preview integration | `DONE` (#11 merged) |
| **PR4**: Remove `BlenderWorker`, `blender_discovery`, `cycles.rs`, `eevee.rs`, `workers/blender/`; native `final_render`, `walkthrough`, `panorama` (equirectangular camera in path tracer) | `DONE` (#12 merged) |
| **PR5 — Task 15**: Native IFC STEP parser with schema detection (IFC2x3 / IFC4 / IFC4x3), streaming iterator, multi-line records & comments, UTF-8 preservation | `DONE` (#13 merged) |
| **PR5 — Task 16**: Native IFC STEP writer with GUID preservation, deterministic numbering, verbatim unknown-Pset round-trip via `PropertyValue::Other` | `DONE` (#13 merged) |
| **PR5 — Task 17**: Native IFC geometry tessellator (IfcExtrudedAreaSolid, IfcFacetedBrep, RectangleProfile / CircleProfile / ArbitraryClosedProfile, IfcBooleanClippingResult) | `DONE` (#13 merged) |
| **PR5 — Task 18**: Delete `workers/ifc/`, remove `python-workers` CI job, audit and update all stale Blender/IfcOpenShell doc references | `DONE` (#13 merged) |
| **PR-A through PR-H5 — DWG codec**: R12 / R14 / R2000 / R2004 / R2007 / R2010 / R2013 / R2018 LibreDWG oracle conformance, OBJECT supertype, HANDSEED, R2007 entity round-trip, shared OBJECTS+HANDLES helpers | `DONE` (#19–#30 merged) |
| **PR-I, PR-I.5 — bridge**: forward-only schema migrations, v2 audit_chain, RwLock singleton, `with_service_ref_fallible`, engine-status connection cache | `DONE` (#31, #32 merged) |
| **PR-J — render fidelity**: aux feature buffers (albedo / normal / depth), MIS for BSDF-found emitters, stratified Halton(2,3) jitter | `DONE` (#33 merged) |
| **PR-K — IFC material library + revolved/swept solids**: bridge N-API surface | `DONE` (#34 merged) |
| **PR-L, PR-M, PR-N — `bim_attach_ifc` service layer**: snapshot cache, IfcMaterialLayerSetUsage round-trip, IFC4-mandatory-fields invariant unrepresentable in `LayerSetUsageKey` | `DONE` (#35, #36, #37 merged) |
| **PR-O — IFC4 polish**: IfcLogical tri-state (true / false / unknown), real-world IFC4 fixture (`small_office.ifc` integration test), pre-parse `bim_check_file_size` guard with renderer confirm dialog | `DONE` (#38 merged) |
| **PR-P — BIM wiring + Phase 9 closer**: `bim_attach_ifc` end-to-end (napi + IPC + electron-bridge + renderer), `IfcBuildingElementProxy` first-class variant for external Revit / ArchiCAD IFCs, `bim_import_ifc` / `bim_check_file_size` / `bim_attach_ifc` promoted from `NATIVE_FALLBACK_METHODS` to `NATIVE_WIRED_METHODS` | `DONE` |
| Phase 6–9 documentation refresh (ARCHITECTURE.md, PROPOSAL.md, README.md, PHASES.md, this file) | `DONE` |

### Exit criteria

- [x] `cargo test --workspace` passes with no Python dependency anywhere in the tree.
- [x] No `workers/blender/` directory.
- [x] No `workers/ifc/` directory.
- [x] No `BlenderWorker`, `BlenderRequest`, `BlenderResponse`, `blender_discovery`, `cycles.rs`, `eevee.rs` references in code.
- [x] CPU + GPU path tracer produce visually equivalent output on a fixed test scene.
- [x] Native STEP parser/writer round-trips GUIDs and all Psets (including unmodeled measure types via `PropertyValue::Other`).
- [x] Documentation (ARCHITECTURE.md, PROPOSAL.md, README.md, PROGRESS.md, PHASES.md) reflects the in-process Rust engine throughout.
- [x] CI workflow no longer installs Blender or IfcOpenShell (the `python-workers` job is gone).
- [x] Native DWG codec passes the LibreDWG oracle gate for R12 / R14 / R2000 / R2004 / R2007 / R2010 / R2013 / R2018.
- [x] `bim_import_ifc`, `bim_check_file_size`, and `bim_attach_ifc` are wired end-to-end through the N-API bridge (no longer in `NATIVE_FALLBACK_METHODS`).
- [x] External IFC files from Revit / ArchiCAD with `IfcBuildingElementProxy` placeholder elements parse and are captured in the project graph (no silent drop in path-(b) element capture).
- [x] `IfcLogicalValue` is a first-class tri-state (`True` / `False` / `Unknown`) distinct from `IfcBoolean`, with reader / writer / Pset codec round-trip coverage.
- [x] Renderer-side file-picker calls `bim_check_file_size` before committing to the multi-second STEP parse, so users get a confirm dialog on files at or above the 100 MB warn threshold rather than a frozen UI.

---

## Phase 10 — N-API bridge completion

**Status:** `DONE`

**Goal:** Promote every `BridgeBackend` method from the in-process fallback set to the native-wired set so every renderer gesture journals through `command_apply` (Immediate transaction, audit chain, undo-able).

### Build

| Item | Status |
|---|---|
| `draftDrawPrimitive` wired through `Command::user(DrawPrimitive)` | `DONE` |
| `draftEditTool` wired through `Command::user(EditTool)` | `DONE` |
| `draftCreateSheet` wired through `Command::user(CreateSheet)` | `DONE` |
| `draftSetLayerState` wired through `Command::user(SetLayerState)` | `DONE` |
| `draftImportDxf` wired (async, `spawn_blocking_napi`, per-entity `dxf_to_primitive` → `command_apply`) | `DONE` |
| `draftExportDxf` wired (project graph → `primitive_to_dxf` → `DxfWriter`) | `DONE` |
| `deliverCreateRevision` wired (`RevisionStore.persist()` with manifest + tracked entities) | `DONE` |
| `deliverListRevisions` wired (`RevisionStore.list()`) | `DONE` |
| `deliverCompareRevisions` wired (`aec_core::version_diff::compare_revisions()`) | `DONE` |
| `NATIVE_FALLBACK_METHODS` is empty | `DONE` |
| `bridge.ts` `BridgeBackend` interface + `adaptNative()` overrides for every promoted method | `DONE` |
| Matching Rust unit tests + vitest coverage for every promoted method | `DONE` |
| Re-baseline of `aec_command::CommandKind` to include `DrawPrimitive` / `EditTool` / `CreateSheet` / `SetLayerState` variants | `DONE` |
| TOCTOU fix on `bim_classify` / `bim_set_property` (read-inside-`BEGIN IMMEDIATE`, `busy_timeout` pragma) | `DONE` |
| AsyncTask split for `ai_plan` / `bim_import_ifc` (`spawn_blocking_napi`, frees Node main thread during long IFC parses) | `DONE` |

### Exit criteria

- [x] `NATIVE_FALLBACK_METHODS` in `apps/desktop/electron/bridge.ts` is the empty array.
- [x] Every `BridgeBackend` method has a matching `#[napi]` export in `crates/aec_bridge/src/napi_api.rs`.
- [x] Every mutating gesture (draft + deliver) routes through `command_apply` so it's auditable and undo-able.
- [x] Renderer-side `adaptNative()` self-check throws on a method that appears in neither the wired nor fallback list — runtime contract is enforced.
- [x] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, and `npm test --workspaces` all pass.

---

## Phase 11 — Real domain depth

**Status:** `DONE`

**Goal:** Replace every remaining stub, scaffold, or "TODO" in the workspace with a real working implementation, with tests exercising the real code paths. Mocks are reserved for cases where the production dependency is genuinely unreachable in CI (e.g. headless GPU, OS-specific thermometers).

### Group A — Wire remaining 9 NATIVE_FALLBACK_METHODS (Tasks 1–9)

| Item | Status |
|---|---|
| Task 1 — `draftDrawPrimitive` | `DONE` |
| Task 2 — `draftEditTool` | `DONE` |
| Task 3 — `draftCreateSheet` | `DONE` |
| Task 4 — `draftSetLayerState` | `DONE` |
| Task 5 — `draftImportDxf` | `DONE` |
| Task 6 — `draftExportDxf` | `DONE` |
| Task 7 — `deliverCreateRevision` | `DONE` |
| Task 8 — `deliverListRevisions` | `DONE` |
| Task 9 — `deliverCompareRevisions` | `DONE` |

### Group B — AI integration + template instantiation (Tasks 10–15)

| Item | Status |
|---|---|
| Task 10 — AI accept-diff → `command_apply` (each `DiffOperation` becomes a `Command::ai(...)`, persisted + auditable + undo-able with `ActorKind::Ai`) | `DONE` |
| Task 11 — AI reject-diff audit logging (`AiAuditLogger::log_rejection`, captures diff hash + tool name + scope + reason) | `DONE` |
| Task 12 — Real template instantiation (`template_to_commands` emits real `CreateWall` + `CreateFloor` + `CreateCeiling` + `CreateRoom` + `SetLighting` + `SaveCamera`; one SQL transaction; forensic sidecar at `audit/template_instantiation.json`) | `DONE` |
| Task 13 — Real filesystem revision snapshot (`.snap` copy of `project.sqlite` + manifest + journal head pointer) | `DONE` |
| Task 14 — Real BLAKE3-based version diff between snapshots (geometry / sheet / schedule_row buckets) | `DONE` |
| Task 15 — Real `BeforeAfterReport` (red-demolition / green-new / orange-modified / dark-grey-unchanged SVG plan overlay + render-pair auto-discovery) | `DONE` |

### Group C — Real CAD + export depth (Tasks 16–22)

| Item | Status |
|---|---|
| Task 16 — DXF round-trip fidelity (layers, blocks with body entities + ATTDEFs + nested INSERT, dim styles, text styles) | `DONE` |
| Task 17 — DWG bridge wiring (`draft_import_dwg` / `draft_export_dwg`, auto-detect AC10xx signature, R12 ↔ R2018) | `DONE` |
| Task 18 — Real PDF sheet export (per-viewport rect clipping, 7 linetypes, all 5 dimension kinds, plot-style override per CTB semantics) | `DONE` |
| Task 19 — Real SVG export (all primitives, layer visibility + colour, linetypes via `stroke-dasharray`, viewport `<clipPath>`, hatch patterns, block expansion, title block) | `DONE` |
| Task 20 — Real glTF 2.0 export (meshes with packed buffer / accessors, PBR materials, perspective cameras, `KHR_lights_punctual`, real GLB container) | `DONE` |
| Task 21 — Real glTF / OBJ asset import (LOD chain `[1.0, 0.25, 0.05]`, PBR thumbnail, BLAKE3 dedup, `import_path()`) | `DONE` |
| Task 22 — Real IFC schedule generation (room area via shoelace, door / window / material schedules into multi-sheet XLSX) | `DONE` |

### Group D — Render + governor + audit hardening (Tasks 23–27)

| Item | Status |
|---|---|
| Task 23 — SQLite-backed `RenderJobStore` for crash recovery (resumes queue from last completed tile/frame) | `DONE` |
| Task 24 — Incremental constraint solver (Newton-Raphson, horizontal / vertical / coincident / parallel / perpendicular / equal-length / tangent / fixed) | `DONE` |
| Task 25 — IES profile parsing (IESNA LM-63, photometric web → sphere integration + lookup-texture bake for path tracer) | `DONE` |
| Task 26 — Audit-chain BLAKE3 verification (`verify_chain` walks `audit/*.jsonl`, pins `blake3(previous_hash ‖ entry_bytes)`; bridge-exposed as `project_audit_verify`) | `DONE` |
| Task 27 — Real per-OS thermal monitor (Linux sysfs / macOS `pmset -g therm` / Windows WMI), governor backs off tile concurrency + AI inference on warm/critical | `DONE` |

### Group E — End-to-end validation (Tasks 28–30)

| Item | Status |
|---|---|
| Task 28 — Phase 2 user-journey e2e (interior designer — apartment renovation: create from template → place furniture → set lighting → save 4 cameras → enqueue 4 renders → export client concept pack) | `DONE` |
| Task 29 — Phase 3 user-journey e2e (drafter — steel detail set: 2D drafting template → DXF import → 4 primitives → Move / Copy / Fillet → 3 sheets with title blocks → export DXF + PDF) | `DONE` |
| Task 30 — Phase 4 user-journey e2e (construction PM — BIM Lite + BOQ: IFC import → validate → uniformat-ii classify → set property → room + door schedules → BOQ XLSX → BIM Lite deliver pack) | `DONE` |

### Exit criteria

- [x] No stub or `todo!()` / `unimplemented!()` survives in production code paths exercised by tests.
- [x] Every Phase 11 task ships unit + integration tests against real data (real DXF / IFC / glTF fixtures, real SQLCipher project databases, real BLAKE3 chains).
- [x] The three end-to-end journey tests (`phase2_journey.rs`, `phase3_journey.rs`, `phase4_journey.rs`) drive the `BridgeService` public API through the user-facing workflow in `PROPOSAL.md` and produce real artifacts on disk.
- [x] Per-OS thermal monitor reports a non-`Unknown` state on every supported OS (Linux sysfs / macOS `pmset` / Windows WMI), and the governor scheduler reduces concurrency on `Warm` and denies admission on `Critical`.
- [x] BLAKE3 audit-chain verifier reports the first broken link on a tampered ledger and passes on an intact ledger.
- [x] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, and `npm test --workspaces` all pass.

---

## Phase 12 — Production depth, KChat local IPC, and viewport pipeline

**Status:** `DONE`

**Goal:** Take every Phase 0–11 surface from "works in tests" to "ships to users". Real local-IPC transport to KChat Desktop, real wgpu viewport pipeline, real deliver-pack content (renders, schedules, sheets, IFC), real LLM sidecar connection, render production depth (FinalRenderPipeline + PBR preview + IES GPU texture + NLM denoiser + progress streaming), and cross-cutting production hardening (project migration with pre-migration backup, memory pressure eviction, SQL-backed undo/redo).

### Group A — KChat Desktop local IPC transport (Tasks 1–5)

| Item | Status |
|---|---|
| Task 1 — `LocalIpcTransport` (UNIX socket / Windows named pipe, JSON-line envelope, reconnect with backoff, heartbeat) | `DONE` |
| Task 2 — `LocalIpcPublisher` (`KChatPublisher` impl, `ingest_reviews` polling, dedup on republish) | `DONE` |
| Task 3 — `KChatDiscovery::probe()` (macOS `~/Library/Application Support/KChat/ipc.sock`, Linux `$XDG_RUNTIME_DIR/kchat/ipc.sock`, Windows `\\.\pipe\kchat-ipc`, `AEC_KCHAT_SOCKET_PATH` override) | `DONE` |
| Task 4 — N-API + Electron IPC wiring (`kchat:publish`, `kchat:status`, `kchat:ingestReviews`) | `DONE` |
| Task 5 — Renderer UI (`PublishCardModal` real publish, `KChatStatusIndicator` in StatusBar, `KChatReviewPanel` in Deliver, Settings page instance info) | `DONE` |

### Group B — Real viewport pipeline (Tasks 6–10)

| Item | Status |
|---|---|
| Task 6 — Real wgpu adapter + device acquisition (`ViewportRenderer::new` with HighPerformance → LowPower → force_fallback fallback chain) | `DONE` |
| Task 7 — `RenderPipeline` (geometry → selection overlay → gizmo → grid, 4 WGSL shaders, MSAA 4x, depth + stencil) | `DONE` |
| Task 8 — `SurfaceManager` off-screen render targets (resize without rebuild, frame coalescing on camera/selection/geometry hash, double-buffered output) | `DONE` |
| Task 9 — Design-mode viewport (`ViewportContainer` ↔ `viewport:requestFrame` / `viewport:resize` / `viewport:mouseEvent`; orbit / pan / zoom via bridge) | `DONE` |
| Task 10 — Draft-mode viewport (orthographic projection, major/minor grid at zoom level, crosshair cursor, snap indicator overlay) | `DONE` |

### Group C — Real deliver pack content (Tasks 11–15)

| Item | Status |
|---|---|
| Task 11 — Real PNGs in deliver packs (look in `<project>/renders/`; fall back to a real Quick-preset render thumbnail; no more 67-byte 1×1 placeholder) | `DONE` |
| Task 12 — Real XLSX (`rust_xlsxwriter` multi-sheet workbook for material schedules and BOQ; rows derived from the project graph) | `DONE` |
| Task 13 — Real PDF sheets (`SheetPdfBuilder` per project `Sheet`, viewport clipping, dimensions, title block, real DXF entities) | `DONE` |
| Task 14 — Real IFC bytes (`IfcWriter::to_string_with_materials` + classifications + properties; validated via `aec_bim::validation` before zipping) | `DONE` |
| Task 15 — Real proposal pack (project name, room/material counts, real `render_sheet_svg_full` plan page, render thumbnails when present, AI cover-page draft from real metadata) | `DONE` |

### Group D — AI sidecar real connection (Tasks 16–20)

| Item | Status |
|---|---|
| Task 16 — Real `LlamaCppAdapter` (spawn `llama-server`, `/health` check, SIGTERM-then-kill shutdown, idle unload, restart-with-backoff on crash) | `DONE` |
| Task 17 — Real `LlamaCppTransport` (`/completion` HTTP client, GBNF grammar in body, streaming with `CancelToken`, 503 retry, 4xx fail-fast) | `DONE` |
| Task 18 — `ToolPlanner::plan()` → real `LlamaCppTransport` (grammar-constrained → `ToolCall` parser → `SafetyValidator` → `DiffOperation`s) | `DONE` |
| Task 19 — `ModelManager` (tier selection from governor, BLAKE3-checksummed download, runtime tier switching without restart) | `DONE` |
| Task 20 — Bridge endpoints wired to real inference (`ai_plan` / `ai_accept_diff` / `ai_reject_diff` / `ai_runtime_status`) | `DONE` |

### Group E — Render pipeline production depth (Tasks 21–25)

| Item | Status |
|---|---|
| Task 21 — Real `FinalRenderPipeline` integration (project graph → `RenderScene` via `MeshCache` + `MaterialLibrary`, camera from `CameraStore`) | `DONE` |
| Task 22 — Real PBR preview wiring (rasterizer as default Design viewport, real-time camera updates, lighting presets) | `DONE` |
| Task 23 — IES GPU lookup texture (photometric web → 1D texture upload; WGSL samples instead of using `representative_cd`) | `DONE` |
| Task 24 — NLM denoiser (uses albedo / normal / depth aux buffers; preset-aware auto-select alongside bilateral) | `DONE` |
| Task 25 — Tile-by-tile render progress streaming (per-tile callback, cancellation at tile boundary, wired through bridge → `RenderQueue` UI) | `DONE` |

### Group F — Cross-cutting production hardening (Tasks 26–30)

| Item | Status |
|---|---|
| Task 26 — `ProjectPackage::open` forward-only migrations + pre-migration backup (`project.sqlite.bak.v{from}-to-v{to}`, fresh opens skipped, schema-version mismatch validated against `manifest.json`) | `DONE` |
| Task 27 — Memory pressure governor (`sysinfo` RSS sampling, `MemorySampler` + `MemoryMonitor` with escalation-only listener dispatch, scheduler denies Critical and halves concurrency on Pressured) | `DONE` |
| Task 28 — SQL-backed undo/redo journal (forward deltas to `undo_journal`, `superseded` flag for redo stack, crash recovery test reopens engine from disk) | `DONE` |
| Task 29 — Phase 5 e2e journey (create project → 4 cameras × standard preset batch → list jobs → persist queue → restart service → restore queue → history compare) | `DONE` |
| Task 30 — Phase 7 e2e journey (create project → publish concept card → ingest 3 review comments → disable KChat → all calls return `KChatError::Disabled` → re-enable → publish revision + dedup on re-ingest) | `DONE` |

### Exit criteria

- [x] KChat publish and review ingest round-trip through a real UNIX-domain-socket / named-pipe IPC against a running KChat Desktop instance (or a discovery-overridden mock server in tests).
- [x] Viewport pipeline produces a real wgpu frame off-screen and the renderer surfaces it without going through a JSON-only fallback.
- [x] Deliver packs contain real renders, real XLSX workbooks, real PDFs, real IFC, and a real SVG plan page — no placeholder bytes.
- [x] AI sidecar can be spawned, health-checked, exercised over `/completion`, and unloaded — every bridge endpoint routes through the real transport.
- [x] Render production depth: FinalRenderPipeline pulls from the project graph; PBR preview is the default Design viewport; IES uses a GPU texture; NLM denoiser ships alongside bilateral; tile progress streams through the bridge.
- [x] `ProjectPackage::open` runs forward-only migrations and takes a pre-migration backup before mutating an out-of-date database.
- [x] Memory monitor classifies Normal / Pressured / Critical and the governor scheduler reduces concurrency or denies admission accordingly.
- [x] Undo / redo journal survives a simulated process crash (drop + reopen).
- [x] Phase 5 and Phase 7 user-journey e2e tests pass end-to-end through `BridgeService`'s public API.
- [x] `cargo test --workspace` (2 214 tests), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, and `npm test --workspaces` (266 tests) all pass.

---

## Phase 7 — Optional KChat integration

**Status:** `DONE`

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
| **Render engines** | Native PBR rasterizer preview, native Rust path tracer (wgpu compute, CPU fallback), native batch / walkthrough / panorama |
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
| PBR rasterized preview latency | < 250 ms per refresh | Medium-tier laptop, typical interior scene |
| Path-traced "Standard" render (1080p interior) | < 90 s | RTX 3060 / Apple GPU 10-core |
| Path-traced "High" render (4K interior) | < 6 min | RTX 4070 / Apple GPU 30-core |
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
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — reference implementation studied for the native Rust STEP parser (not a runtime dependency)
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — path-traced renderer

---

## Changelog

### 2026-05-27 (Phase 13 — renderer UX wiring, placeholder elimination, file-picker integration, active project state, UI polish)

- **Group A — Wire renderer pages to real project data (Tasks 1–6).** `apps/desktop/renderer/src/pages/Bim.tsx` now reads the active project via `useActiveProject()` and threads `project.path` through every `aec.bim.*` call. `bim:readScheduleRows` IPC handler round-trips the generated XLSX so the renderer displays real schedule rows (Task 2). `Design.tsx` consumes `useActiveProject()` so the route guard fires; design commands resolve project path on the main-process side via `withResolvedProjectPath` in `ipc.ts` (Task 3). `Draft.tsx` gains real DXF/DWG import/export wired through new file-picker IPC and `aec.draft.importDxf` / `aec.draft.exportDxf` (Task 4). `Deliver.tsx` builds packs through a save dialog, threads `projectName` through, and surfaces success/error toasts (Task 5). `Render.tsx` loads cameras from the project graph (`aec.command.listGraph(projectPath, "camera")`), falls back to demo cameras only when the project has none yet, enqueues real render jobs, and supports best-effort cancellation (Task 6). All `demo://` placeholder paths are removed from production code.
- **Group C — File-picker and dialog integration (Tasks 13–17).** New `apps/desktop/electron/dialog-ipc.ts` ships `dialog:openFile`, `dialog:openDirectory`, `dialog:saveFile` IPC handlers wrapping Electron's `dialog.show*Dialog` with full parameter validation and a `__setDialogModule` test hook. Wired into BIM (IFC import, attach, export), Draft (DXF/DWG import/export), Deliver (ZIP pack save dialog), and Home (project open / recents list with stale-entry cleanup).
- **Group D — Active project state management (Tasks 18, 19; 20–22 partial).** New `apps/desktop/renderer/src/hooks/useActiveProject.tsx` holds the open `ProjectSummary`, exposes `openProject(path)`, `createProject(template, name)`, `closeProject()`, `saveProject()`, dirty / saving / undo / redo flags, and a debounced auto-save (5 s after last `markDirty()`). `App.tsx` wraps the tree in `<ToastProvider><ActiveProjectProvider>` and route-guards every mode page with `<RequireProject>` (redirects to `/` if no project is open). Project name displayed in the app header. Keyboard shortcuts: Ctrl/Cmd+Z (undo), Ctrl/Cmd+Shift+Z (redo), Ctrl/Cmd+S (save), Ctrl/Cmd+W (close project), all scoped by the current route via `useLocation` → `CommandScope`.
- **Group E — UI polish (Tasks 23, 24, 25, 26, 27).** New `useToast.tsx` toast provider (success/info auto-dismiss 5 s, error persists). New `StatusBar.tsx` wiring: project name + dirty/saving/saved indicator + undo/redo stack depths + active render job count (polled 5 s) alongside the existing GPU/RAM/tier chips. New `ShortcutHelp.tsx` overlay listing every registered shortcut grouped by group; triggered by Shift+? or Ctrl/Cmd+/, reads live from `shortcutRegistry`. New `ErrorBoundary.tsx` wraps each mode page so a runtime exception in one mode doesn't crash the shell; surfaces the error message + a retry button. `ViewportContainer.tsx` ResizeObserver coalesces resize bursts with a 120 ms trailing edge so the bridge surface is re-allocated once per drag instead of 30×/s; resolution indicator in the bottom-right updates optimistically during drags.
- **IPC surface additions.** `project:current`, `project:close`, `bim:readScheduleRows`, `dialog:openFile`, `dialog:openDirectory`, `dialog:saveFile`. `active-project.ts` extended from path-only tracking to full `ActiveProjectSummary` (path + cached summary) with `setActiveProject(summary)`, `peekActiveProjectSummary()`, `onActiveProjectChange(listener)` subscriber API.
- **Tests.** All 281 vitest tests pass (52 files). New specs for `ErrorBoundary`, `ShortcutHelp`, `useToast`. Existing page tests (`Bim`, `Deliver`, `Draft`, `Home`, `Render`, `ScheduleView`, `ValidatorPanel`, `App`) updated to wrap in `<ToastProvider><ActiveProjectProvider>` and use real-shaped paths.
- **Deferred to Phase 13 follow-up:** Group B (Tasks 7–12, backend placeholder elimination + real `DeliverPackContext` from project state) and Group F (Tasks 28–30, acceptance criteria benchmarks + Phase 6/8 e2e journey tests) ship in a follow-up PR. They are pure backend / test additions with no UI implications, so splitting them keeps the renderer-side review cycle focused.

### 2026-05-27 (Phase 13 follow-up — Group B + Group F: backend placeholder elimination + performance validation)

- **Group B — Eliminate backward-compat placeholder paths (Tasks 7–12).** New `aec_bridge::pack_context` module owns the `OwnedPackContext` struct + the stateless `build_for_project()` reconstructor that hydrates real project state from SQLCipher on demand (renders dir scan, attached IFC parse via `IfcReader`, project metadata via `ProjectSummary`, schedule generation via `aec_bim::schedules::generate_material_schedule`). The bridge's `deliver_build_pack` and `export_proposal_pack` endpoints now route through `*_with_context` variants with real project state — the no-context library APIs `write_deliver_pack` / `write_proposal_pack` are kept `#[deprecated]` for backward compatibility with external embedders but every production code path in the bridge constructs and supplies a real `OwnedPackContext`. `placeholder_xlsx()` has been **deleted entirely** from `aec_export::project_export`; all production callers route through `empty_real_xlsx()` / `build_real_xlsx()` (real OpenXML via `rust_xlsxwriter`) so a missing-schedule source degrades to a real-but-empty workbook, never to a hand-rolled placeholder ZIP. `placeholder_png()` had already been removed in Phase 12. IPC + napi surfaces grew a new `project_path: string` parameter on `proposal:export` (electron `ipc.ts` resolves it through the active-project bridge before passing to napi); the renderer path was already routing `useActiveProject().path` through to the IPC call from PR #70.
- **Group F Task 28 — Real acceptance-criteria benchmarks.** New `crates/aec_bridge/benches/acceptance_criteria.rs` Criterion harness with 4 benches against the `PROPOSAL.md` targets: cold start (`BridgeService::new()` + first `runtime_status()`, target ≤2.5s, measured 40ms — 60× faster than target), project open from apartment template (target ≤1.0s, measured 6ms — 165× faster), DXF import of a 10k-LINE fixture (target ≤1.5s), and AI runtime status (target ≤1.5s). Each iteration creates a fresh tempdir + copies the apartment template so cold-start is not measuring filesystem cache. `sample_size = 10` per bench for confidence without excessive CI runtime.
- **Group F Task 29 — Phase 6 e2e journey test.** New `crates/aec_bridge/tests/phase6_journey.rs` walks the full deliver workflow: create project from apartment template → model 2 named rooms (Living, Bedroom) via `CreateWall` + `CreateRoom` commands → save 2 named cameras (Living POV, Bedroom POV) via `SaveCamera` → enqueue 2 render jobs (one per camera × standard preset) → create revision v1 via `deliver_create_revision` → add diff wall → create revision v2 → compare v1 vs v2 via `deliver_compare_revisions` (asserts non-empty `changes` + `by_category` diff) → export all 4 pack kinds (concept, interior, contractor, bim). Each pack is validated by reopening the ZIP: archive is parseable, manifest entries are present, and (for contractor/bim) the XLSX entry is a real OpenXML workbook (PK\x03\x04 header). Robust to template-seeded entities — uses name-based filtering rather than absolute count assertions.
- **Group F Task 30 — Phase 8 extension lifecycle e2e test + new `extensions_install_asset_packs` bridge endpoint.** New `BridgeService::extensions_install_asset_packs(extensions_dir, require_signature)` endpoint that loads an extensions directory through `aec_core::extensions::ExtensionLoader`, constructs a `PermissionEnforcer` from the registry, and routes `aec_assets::extension_host::install_asset_packs` against the bridge's `AssetState` DB via `with_db_mut`. New `crates/aec_bridge/tests/phase8_journey.rs` exercises the full lifecycle: authors a real on-disk AssetPack extension (manifest + payload bytes with BLAKE3 hashes) → installs it via the bridge → verifies the new assets are visible through `design_list_assets` (filtered by tag) → re-runs install to confirm idempotence (all entries on `skipped` list) → authors a tampered extension (payload overwritten after manifest) and asserts the install fails with a checksum error → authors an extension missing `filesystem_read` and asserts the install fails with a permission error.
- **Lint / format / type-check.** `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --all --check` clean, `npm run lint --workspaces` 0 errors (14 pre-existing warnings about `react-refresh/only-export-components` on context helpers — same as PR #70 baseline), `npm run type-check --workspaces` clean.
- **Tests.** 341 vitest passing (56 files) — same as PR #70 baseline + 0 new specs (renderer surface unchanged). Full `cargo test --workspace` passing including 3 new bridge integration tests (`phase13_real_context`, `phase6_journey`, `phase8_journey`) and 1 new Criterion bench suite (`acceptance_criteria`).

### 2026-05-27 (Phase 12 — production depth, KChat local IPC, viewport pipeline, 30 tasks in one PR)

- **Group A — KChat Desktop local IPC.** New `crates/aec_core/src/kchat_transport.rs` ships a `LocalIpcTransport` speaking a discriminated-union JSON-line envelope (`ping` / `publish` / `ingest_reviews`) over a UNIX domain socket on macOS/Linux and a named pipe on Windows, with a hand-rolled 5-step exponential reconnect schedule and a `seconds_since_heartbeat()` accessor for the StatusBar's freshness indicator. `crates/aec_core/src/local_ipc_publisher.rs` wraps it as a `KChatPublisher` with a `(thread_id, project_link, caption)` dedup set so double-clicking the publish modal is a no-op. `crates/aec_core/src/kchat_discovery.rs` walks the platform-canonical paths (macOS Application Support, Linux `$XDG_RUNTIME_DIR`, Windows pipe namespace) and an `AEC_KCHAT_SOCKET_PATH` test override. Bridge exports `kchat_publish` / `kchat_status` / `kchat_ingest_reviews`; Electron IPC handlers `kchat:publish` / `kchat:status` / `kchat:ingestReviews` route through `preload.ts` and `bridge.ts`. Renderer ships `PublishCardModal` (real publish wired), `KChatStatusIndicator` in StatusBar with green/yellow/red dot, `KChatReviewPanel` in Deliver showing ingested comments with thread context, and KChat Desktop instance info on the Settings page.
- **Group B — Real viewport pipeline.** `ViewportRenderer::new` requests an adapter with the HighPerformance → LowPower → `force_fallback_adapter` chain and stores `device`, `queue`, and `adapter_info` non-optionally on success. New `crates/aec_viewport/src/render_pipeline.rs` wires the four existing WGSL shaders (geometry, selection, gizmo, grid) into a forward-rendering pipeline with 4× MSAA, a depth buffer, and a selection stencil. New `crates/aec_viewport/src/surface.rs` allocates off-screen render targets, supports resize without rebuilding the pipeline, and frame-coalesces on `(camera_hash, selection_hash, geometry_hash)` so an idle viewport isn't redrawn at 60 Hz. `ViewportContainer` requests frames via `viewport:requestFrame` and routes mouse events to the bridge via `viewport:mouseEvent`; `DraftCanvas` shares the pipeline with an orthographic camera.
- **Group C — Real deliver pack content.** `write_deliver_pack` now scans `<project>/renders/` for real PNG bytes (falling back to a real Quick-preset path-traced thumbnail rather than the 67-byte 1×1 placeholder), produces a multi-sheet XLSX via `rust_xlsxwriter` for material schedules and BOQs, renders each project `Sheet` via `SheetPdfBuilder` with real DXF entities clipped to the viewport, serialises the project graph to IFC via `IfcWriter::to_string_with_materials` (validated through `aec_bim::validation` before zipping), and emits a proposal pack with real project metadata, a real `render_sheet_svg_full` plan page, and an AI cover-page draft derived from the project's room/material/lighting state.
- **Group D — AI sidecar real connection.** `crates/aec_ai/src/sidecar.rs` spawns `llama-server` as a child process, polls `/health` with a 30-attempt readiness loop, terminates with SIGTERM (force-kills after a timeout), unloads after 60 s of idle, and restarts on crash with backoff. `crates/aec_ai/src/transport.rs` posts to `/completion` with the GBNF grammar in the request body, streams tokens with `CancelToken`, retries 503 (model loading), and fails fast on 4xx. `ToolPlanner::plan()` routes through the real transport, parses tool calls, and validates them through `SafetyValidator` before producing `DiffOperation`s. `ModelManager` selects a tier from the governor's hardware profile, downloads model files with BLAKE3 checksum verification, and supports runtime tier switching without a restart. Bridge endpoints `ai_plan`, `ai_accept_diff`, `ai_reject_diff`, and `ai_runtime_status` are wired end-to-end.
- **Group E — Render pipeline production depth.** `final_render.rs` builds a `RenderScene` from real project entities (walls, furniture, materials) via `aec_geometry::MeshCache` + `aec_materials::MaterialLibrary` and applies camera parameters from `CameraStore`. `pbr_preview.rs` is the default Design-mode viewport renderer; orbit triggers an immediate re-render; lighting presets (sun position, sky colour, ambient) come from `aec_render::lighting`. The GPU path tracer now samples IES profiles from a 1-D lookup texture baked from the photometric web instead of approximating with `representative_cd`. `denoise.rs` ships an NLM denoiser alongside the bilateral one, auto-selecting by sample count via `Denoiser::auto_for_samples` (NLM for ≤ 64 spp / Quick presets, bilateral for higher sample counts where NLM's edge-preservation gain shrinks below its cost). `render_or_fallback` streams per-tile progress through a callback channel and supports cancellation at tile boundaries.
- **Group F — Cross-cutting production hardening.** `ProjectPackage::open` runs forward-only migrations from `crates/aec_core/src/migrations/` against a pre-migration `project.sqlite.bak.v{from}-to-v{to}` backup (`crates/aec_core/src/db.rs::backup_before_migration` uses raw `std::fs::copy` so the encrypted SQLCipher backup is bit-identical and decryptable with the same key). `crates/aec_governor/src/memory.rs` ships `MemorySampler` + `MemoryMonitor` polling `sysinfo` on a 5-second cadence; the scheduler denies new admissions at `Critical` and halves tile concurrency at `Pressured`, and the StatusBar surfaces the current band. `CommandEngine::undo` / `redo` now persist forward deltas to the SQL `undo_journal` table with a `superseded` flag for the redo stack; a `crash_recovery` test drops the engine, reopens it from disk, and confirms the undo stack survives. New `crates/aec_bridge/tests/phase5_journey.rs` exercises the full Phase 5 render workflow (create → 4 cameras × standard preset batch → persist queue → fresh service boot → restore → history `compare(a, b)` reporting `preset_changed` / `camera_changed` / `image_hash_changed` / `duration_delta_ms`). New `crates/aec_bridge/tests/phase7_journey.rs` exercises the full Phase 7 KChat collaboration journey against a real UNIX-socket mock server (publish concept render → ingest 3 review comments → audit entries with `ActorKind::KChat` → disable KChat → all calls return `KChatError::Disabled` *before* hitting the wire → re-enable → publish revision pack → `CommentSync` dedupes on re-ingest).
- **Tests.** `cargo test --workspace` passes 2 214 tests across all crates; `cargo clippy --workspace --all-targets -- -D warnings` is clean; `cargo fmt --all --check` is clean; `npm test --workspaces` passes 266 tests across `apps/desktop/electron` and `apps/desktop/renderer`.

### 2026-05-25 (Phase 10 + Phase 11 — N-API bridge completion + real domain depth, 30 tasks across PRs #48–#66)

#### Phase 10 — bridge completion (PR #48)

- **9 `NATIVE_FALLBACK_METHODS` promoted to `NATIVE_WIRED_METHODS`.** `draftDrawPrimitive`, `draftEditTool`, `draftCreateSheet`, `draftSetLayerState`, `draftImportDxf`, `draftExportDxf`, `deliverCreateRevision`, `deliverListRevisions`, `deliverCompareRevisions`. Each method now has a matching `#[napi]` export in `crates/aec_bridge/src/napi_api.rs`, a service-layer implementation in `crates/aec_bridge/src/service.rs`, a `BridgeBackend` interface entry in `apps/desktop/electron/bridge.ts`, and an `adaptNative()` override unwrapping the N-API JSON envelope back into the renderer's typed shape. `NATIVE_FALLBACK_METHODS` is now the empty array; the `adaptNative()` self-check throws on any method that appears in neither list, so the wiring contract is enforced at runtime.
- **New mutating-gesture command variants.** `aec_command::CommandKind` gained `DrawPrimitive`, `EditTool`, `CreateSheet`, `SetLayerState`. Each variant implements `apply()` against a `CommandEngine` session, serialises deltas to the journal, and is undo-able.
- **DXF I/O.** `crates/aec_cad/src/dxf/convert.rs` exposes `primitive_to_dxf(&Primitive) → Option<DxfEntity>` and the inverse `dxf_to_primitive(&DxfEntity) → Option<Primitive>` covering Line / Polyline / Circle / Arc / Ellipse / Text; the bridge's `draft_import_dxf` is async (`spawn_blocking_napi`) because real DXF files routinely cross 10 MiB.

#### Phase 11 — real domain depth (PRs #49–#66)

**Group A — bridge fallbacks (Tasks 1–9, PR #48).** Listed above under Phase 10.

**Group B — AI + templates + revisions (Tasks 10–15, PRs #49–#53).**

- **Task 10 + 11 (PR #49).** `ai_accept_diff` now applies every `DiffOperation` through `command_apply` with `ActorKind::Ai` attribution, so AI edits are persisted, audit-chained, and undo-able. `ai_reject_diff` logs to `AiAuditLogger::log_rejection` with the diff hash, tool name, scope, and rejection reason.
- **Task 12 (PR #51).** `TemplateLoader::load()` followed by `template_to_commands(&TemplateDefinition)` emits real `CreateWall`, `CreateFloor`, `CreateCeiling`, `CreateRoom`, `SetLighting`, and `SaveCamera` commands; the batch is persisted in one SQL transaction via `execute_persistent_batch` (failed batch rolls the project directory back). A forensic sidecar at `<project>/audit/template_instantiation.json` captures entity ids per room and any skipped rooms. 9 templates instantiate cleanly: apartment, café, office, villa, retail, kitchen, bathroom, renovation, 2d_drafting.
- **Task 13 + 14 (PR #52).** `RevisionStore::create_with_snapshot` writes a real `.snap` copy of `project.sqlite` plus a manifest and the journal head pointer for replay. `aec_core::version_diff::compare_revisions` walks two snapshots and produces a BLAKE3-bucketed `VersionDiff` with `geometry` / `sheet` / `schedule_row` change sets.
- **Task 15 (PR #53).** `aec_export::BeforeAfterReport` generates a real SVG plan overlay (red demolition / green new / orange modified / dark-grey unchanged) from two SQLCipher project snapshots, plus auto-discovery of paired render files in the project's `renders/` directory.

**Group C — CAD + export depth (Tasks 16–22, PRs #54–#60).**

- **Task 16 (PR #54).** DXF reader/writer round-trips every entity attribute: layers (on/off, plottable, description on top of color/lineweight/linetype/frozen/locked), blocks with body entities + base points + nested INSERTs + ATTDEFs, `DxfTextStyle` (font, bigfont, fixed_height, width_factor, oblique_angle), `DxfDimStyle` (decimal_places + text_style on top of the existing fields). Real-world fixture at `crates/aec_cad/tests/fixtures/roundtrip_full.dxf` pins byte-stable `serde_json` equality.
- **Task 17 (PR #55).** `draft_import_dwg` auto-detects the `AC10xx` 6-byte signature, lowers through `DxfDocument`, and journals every entity through `command_apply_batch`. `draft_export_dwg` accepts a `version_signature` and validates the AC tag (wrong-length / unknown tag both surface as `BridgeServiceError::Invalid`). Both async via `spawn_blocking_napi`.
- **Task 18 (PR #56).** Real PDF sheet export with per-viewport rect clipping (`save_graphics_state` / `clip` / `restore` bracket), 7 well-known linetypes (Continuous / Dashed / Hidden / Center / Phantom / DashDot / Divide), full dimension rendering for all 5 `DxfDimensionKind` variants (extension lines + dim line + arrowheads + measured value formatted at the dim style's decimal places), per-viewport `frozen_layers` honoured, plot-style override per CTB semantics. Ellipse + ATTDEF now render (no more no-op branches).
- **Task 19 (PR #57).** Real SVG export covering every previously-skipped entity (Ellipse, Spline, Insert, Dimension), layer visibility & colour, per-viewport `frozen_layers`, linetypes via `stroke-dasharray`, viewport `<clipPath>` clipping (toggleable), named hatch patterns via `<pattern>` defs, block expansion against the `BlockTable`, and the sheet's title block.
- **Task 20 (PR #58).** Real glTF 2.0 export with meshes (POSITION / NORMAL / TEXCOORD_0 / indices packed into one binary buffer with buffer-view + accessor wiring; index type auto-narrows u16↔u32; mm→metres on write), PBR metallic-roughness materials, perspective cameras with lookAt→(translation, quaternion) decomposition, and `KHR_lights_punctual` lights. `.glb` writes a real GLB container (12-byte header + JSON chunk + BIN chunk per spec).
- **Task 21 (PR #59).** Real glTF / OBJ asset import: spec `[1.0, 0.25, 0.05]` LOD chain via mesh decimation, PBR thumbnail rasterisation, BLAKE3 dedup on re-import, content-addressed asset DB, one-shot `import_path()` entry point.
- **Task 22 (PR #60).** Real IFC schedules generated from project data: room (with area via shoelace), door, window, and material schedules into a multi-sheet XLSX via the bridge's `bim_generate_schedule`.

**Group D — render + governor + audit hardening (Tasks 23–27, PRs #61–#65).**

- **Task 23 (PR #61).** SQLite-backed `RenderJobStore` persists every render job's state to the project's `renders/` directory; on crash, the render queue resumes from the last completed tile/frame.
- **Task 24 (PR #62).** Incremental Newton-Raphson constraint solver supporting horizontal, vertical, coincident, parallel, perpendicular, equal-length, tangent, and fixed constraints, with an incremental drag-from-handle entry point that re-solves only the affected sub-graph.
- **Task 25 (PR #63).** IESNA LM-63 parser extracts the photometric web into a sphere-integrated lookup texture for the path tracer's light sampling.
- **Task 26 (PR #64).** Audit-chain BLAKE3 verifier (`verify_chain(audit_dir) -> ChainVerification`) walks every `audit/*.jsonl` chronologically and pins `blake3(previous_hash ‖ entry_bytes)`; reports the first broken link if any. Exposed through the bridge as `project_audit_verify`.
- **Task 27 (PR #65).** Real per-OS thermal monitor: Linux reads `/sys/class/thermal/thermal_zone*/temp` filtered to CPU zones; macOS shells to `/usr/bin/pmset -g therm` and parses `CPU_Speed_Limit`; Windows queries WMI `MSAcpi_ThermalZoneTemperature` via PowerShell. `ThermalMonitor` polls on a 5 s schedule and applies a `ThermalState` (Nominal / Warm / Critical) to `GovernorScheduler` — Warm halves render-tile concurrency, Critical denies all admissions with `BackoffReason::Thermal` and pauses AI inference.

**Group E — end-to-end user-journey integration tests (Tasks 28–30, PR #66).**

- **Task 28 — `crates/aec_bridge/tests/phase2_journey.rs`.** Interior-designer "apartment renovation in a weekend" journey driven through `BridgeService`: `project_create_from_template("interior.apartment", ...)` → place 4 furniture via `command_apply(PlaceFurniture)` → set lighting preset → save 4 cameras → enqueue 4 renders via `render_enqueue_batch` → export client concept pack via `deliver_build_pack`. Companion test verifies state persists across a service restart.
- **Task 29 — `crates/aec_bridge/tests/phase3_journey.rs`.** Drafter "pure 2D CAD for a steel detail set" journey: `project_create_from_template("drafting.2d_drafting", ...)` → import vendor DXF via `DxfReader::read_str` → draw 4 primitives (Line / Polyline / Arc / Circle) via the real `aec_cad::primitives` API → edit (Move / Copy / Fillet) via the real `aec_cad::editing` tools → assemble 3 sheets with title blocks via `aec_cad::sheets::Sheet` → export DXF + PDF via the bridge's `export_dxf` / `export_pdf`. Companion test pins DXF write-then-read fidelity for the four primitive types the drafter actually draws.
- **Task 30 — `crates/aec_bridge/tests/phase4_journey.rs`.** Construction-PM "site renovation with BIM Lite and BOQ" journey: `project_create_from_template` → `bim_import_ifc` against `small_office.ifc` fixture → `bim_attach_ifc` → `bim_validate` → `command_apply(CreateWall)` seed → `bim_classify("uniformat-ii")` (asserts the seeded wall maps to Uniformat B2010) → `bim_set_property(Pset_WallCommon, FireRating="REI120")` → room + door schedules via `bim_generate_schedule` → contractor BOQ pack via `deliver_build_pack(kind="contractor", include_boq=true)` → BIM Lite pack via `deliver_build_pack(kind="bim")`. Companion test verifies all 4 supported schedule kinds (door / window / room / material) are independently addressable.

#### Tests + lint

- `cargo test --workspace` (with `XDG_RUNTIME_DIR=/tmp` for wgpu): all passing (60+ binaries, including new `phase2_journey`, `phase3_journey`, `phase4_journey`).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `npm test --workspaces`: passing (renderer + electron unit tests cover the new bridge surface).

### 2026-05-20 (Phase 9 — BIM wiring, proxy-element gap, doc closer, PR-P)

- **`bim_attach_ifc` wired end-to-end** through the native bridge. New `BimAttachSummaryJs` and `bim_attach_ifc(project_path, ifc_path)` `#[napi]` exports in `crates/aec_bridge/src/napi_api.rs` (routed via `with_service_ref_fallible`, same locking pattern as `bim_import_ifc`); matching `BimAttachSummary` interface, `bim:attachIfc` IPC handler, and `preload.ts` / `bridge.ts` / renderer-side `bim-attach.ts` helper. The renderer's BIM toolbar gains an "Attach IFC" button; in production the IPC dispatches into the snapshot-cache-backed Rust service (a recent `bim_import_ifc` call on the same path serves the parse for free, reported as `parseCacheHit: true` in the result).
- **`IfcBuildingElementProxy` promoted to a first-class `IfcClass` variant.** Pre-PR-P, external IFCs from Revit (custom families exported as proxies), ArchiCAD (MEP federations), and buildingSMART exemplars that used `IFCBUILDINGELEMENTPROXY` for "real building element but doesn't fit a precise IFC subtype" fell into the `IfcClass::Other(_)` arm and were silently dropped by the path-(b) `Other(_)` guard in `IfcReader::collect_elements_by_step_tag`. The new variant is recognised by `ifc_tag()`, `ifc_class_from_tag()`, and `is_building_element()`, so those elements now land in the project graph. Round-trip pinned by `ifc_building_element_proxy_roundtrips_as_first_class_variant`; path-(b) capture pinned by `external_ifc_captures_ifcbuildingelementproxy_via_path_b`.
- **`bim_import_ifc`, `bim_check_file_size`, and `bim_attach_ifc` promoted from `NATIVE_FALLBACK_METHODS` to `NATIVE_WIRED_METHODS`** in `apps/desktop/electron/bridge.ts`. The `adaptNative` self-check throws on a method that appears in neither list, so this also pins the wiring contract at runtime. Closes the no-op size-guard Devin Review found in PR-O round 1 — the confirm dialog will now actually fire in the built app.
- **PHASES.md + PROGRESS.md updated.** Phase 9 status flipped to `DONE`; the PR-A → PR-P timeline is reflected in the per-item table; four new Phase-9 exit criteria covering DWG oracle conformance, BIM-method wiring, proxy-element capture, and the renderer-side size guard are now checked.
- **Tests.** New: `bim-attach.test.ts` (7), `ifc_building_element_proxy_roundtrips_as_first_class_variant`, `ifc_building_element_proxy_classifies_as_building_element`, `external_ifc_captures_ifcbuildingelementproxy_via_path_b`. Updated: `bim-import.test.ts` mocks now return the full `BimImportSummary` shape; `BimToolbar.test.tsx` covers the new `attachIfc` action; `ifc_class_other_roundtrips_through_writer_and_reader` switched to a synthetic `IfcAecStudioCustomComponent` identifier so it still pins the `Other(_)` round-trip after the proxy promotion. `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all --check` + `npm run -w apps/desktop test` (183 vitest) all green.

### 2026-05-20 (Phase 9 — native render & BIM engine, PRs #9–#13)

- **Removed all external runtime dependencies for rendering and BIM.** The Blender worker (Python scripts driving Blender for EEVEE/Cycles, JSON-line IPC over stdin/stdout, `workers/blender/`) and the IfcOpenShell worker (Python scripts wrapping IfcOpenShell, `workers/ifc/`) are gone. Rendering and IFC handling are now fully in-process Rust.
- **PR1 #9 — CPU path tracer core.** SAH BVH2 builder with two-level instancing (`crates/aec_render/src/bvh.rs`), Möller–Trumbore ray-tri intersection + stack-based traversal (`intersect.rs`), CPU path tracer with Russian roulette and next-event estimation (`path_trace.rs`), principled BSDF with GGX + Schlick (`material.rs`), and MIS light sampling for sun / area / point / sky + IES profiles (`light_sampling.rs`).
- **PR2 #10 — GPU compute path tracer + denoiser + scheduler.** WGSL compute kernel (`shaders/path_trace.wgsl`) mirroring the CPU path: BVH traversal, principled BSDF, light sampling, MIS. Edge-aware bilateral denoiser (`denoise.rs`). Tile scheduler with progressive sampling, adaptive convergence, and cancellation (`scheduler.rs`).
- **PR3 #11 — Native PBR preview + sky.** wgpu PBR rasterization pipeline (`crates/aec_viewport/src/pbr_preview.rs` + `shaders/pbr.wgsl`) with cascaded shadow maps and SSAO; Hosek-Wilkie procedural sky (`sky.rs` + `shaders/sky.wgsl`); `PreviewPipeline` (replacing `EeveePipeline`) drives the same pipeline for the editor preview.
- **PR4 #12 — Removed Blender, native walkthrough + panorama.** Deleted `crates/aec_render/src/worker.rs` (BlenderWorker), `blender_discovery.rs`, `cycles.rs`, `eevee.rs`, and the entire `workers/blender/` tree. New `final_render.rs` drives the path tracer end-to-end; `walkthrough.rs` renders camera-path frame sequences with frame-level resume and optional ffmpeg stitch; `panorama.rs` adds an equirectangular camera projection to the path tracer for 360° room panoramas.
- **PR5 #13 — Native IFC engine.** New `crates/aec_bim/src/ifc/reader.rs` (STEP parser with `FILE_SCHEMA(('IFC2X3'|'IFC4'|'IFC4X3'))` detection, streaming `StepIter`, multi-line records, ISO 10303-21 comments, UTF-8-safe quoted-string parsing); `writer.rs` (deterministic GUID-preserving STEP emitter, verbatim unknown-Pset round-trip via `PropertyValue::Other`); `tessellator.rs` (IfcExtrudedAreaSolid, IfcFacetedBrep, RectangleProfile / CircleProfile / ArbitraryClosedProfile, IfcBooleanClippingResult). Deleted `workers/ifc/` and the `python-workers` CI job.
- **Documentation refresh.** ARCHITECTURE.md, PROPOSAL.md, README.md, PROGRESS.md, and PHASES.md updated to reflect that rendering and BIM are in-process Rust. `docs/LICENSE_ARCHITECTURE.md` notes the GPL (Blender) and LGPL (IfcOpenShell) boundaries are no longer relevant — only the AGPL (project source) and MIT (llama.cpp) boundaries remain.
- **Tests.** `cargo test --workspace` stays green throughout the migration; `cargo clippy --all-targets --all-features -- -D warnings` clean.

### 2026-05-20 (CI gating — Ubuntu-only PR baseline, full matrix on `main`)

- **CI workflow gating.** `.github/workflows/ci.yml` now drives the `rust` and `typescript` job matrices off a small `gate` job that computes the OS list at run time. Default policy: PRs run an **Ubuntu-only** baseline (Rust + TypeScript + Python workers) so platform-specific runner flakes (e.g. occasional Electron CDN 404s on `macos-latest` during `npm ci`) can never block a PR that didn't touch platform code; the **full `ubuntu-latest` + `macos-latest` + `windows-latest` matrix** runs unconditionally on every push to `main`, which is the merge gate for the next release. Opt-in label `test-all-platforms` flips a PR into the full matrix when a contributor intentionally touches Electron, packaging configs, or OS-specific Rust code. Documented in `CONTRIBUTING.md` ("Pass CI" section) so the policy is discoverable from the contributor flow rather than buried in workflow YAML.

### 2026-05-20 (Phase 8 — extension system)

- **Extension system foundation.** New `crates/aec_core/src/extensions.rs` implementing the PROPOSAL.md §8 manifest schema end-to-end: `ExtensionManifest` with six type-specific bodies (`AssetPackBody`, `TemplateBody`, `ScheduleBody`, `ExportTargetBody`, `AiToolBody`, `ImporterBody`), `ExtensionLoader` that scans `extensions/<id>/manifest.json`, `ExtensionRegistry` indexed by id + kind with `find_template` / `find_schedule` / `find_export_target` / `find_ai_tool` accessors, and `canonical_payload_bytes` returning byte-stable JSON for signing.
- **Permission enforcer + Ed25519 signatures.** New `crates/aec_core/src/extension_permissions.rs` ships `validate_manifest` (required fields, permission set, body shape, AI tool grammar/scope, duplicate detection), `Operation` / `PermissionCheck` / `PermissionEnforcer`, a `TrustStore` of allowed public keys, and two verification paths: `verify_signature_against` (production — requires the key to be in the trust store) and `verify_signature_self_consistent` (dev-only). `keygen_test_only` uses `getrandom` so we avoid the `rand_core` major-version split.
- **Per-type hosts.** Each extension type now has a real host integration: `aec_assets::install_asset_packs` validates the BLAKE3 of every asset payload before writing to `AssetDatabase` and is idempotent on re-run; `aec_core::TemplateLoader::{load_with_extensions, discover_with_extensions}` composes extension templates with the on-disk `templates/` tree (extensions win on key collision, category stamped if JSON omits it); `aec_bim::schedules::extension_host` returns a default-row `ScheduleSheet` per declared schedule extension; `aec_export::extension_targets` resolves `ExtensionExportTarget` descriptors with a typed `ExportFormat`; `aec_ai::extension_tools` resolves `ExtensionAiToolSchema` with parsed `Scope` vec and exposes `enforce_max_entities_modified` as the extension half of `safety_validator`. Every host runs through `PermissionEnforcer::check_permission` before doing any privileged work.
- **Documentation.** New `EXTENSIONS.md` documents the manifest schema, permission model, Ed25519 signing flow, security model, and development guide for every extension type. README / PROPOSAL / ARCHITECTURE link to it from their Links sections.
- **Tests.** 33 new unit tests across the five new modules — manifest serde roundtrip, loader rejects unknown permissions / scopes / duplicate keys, registry lookup by id and kind, asset host idempotency / checksum mismatch / permission denial, template loader extension overlay, schedule sheet defaults, export target format mapping, AI tool scope parsing + safety cap enforcement, signature verify-against vs self-consistent, trust store untrusted-key rejection. `cargo build --all-targets` clean; `cargo test --workspace` stays green.

### 2026-05-20 (Phase 7 + validation batch)

- **Phase 2/5/6 exit criteria validated.** Added `crates/aec_export/tests/phase6_e2e.rs` (single project → concept, interior, contractor, BIM packs), `crates/aec_export/tests/determinism.rs` (PDF content stripped of XMP / dates / xref is byte-identical across runs; XLSX entry inventory + BLAKE3 hashes match), and `crates/aec_export/tests/contractor_perf.rs` (realistic 12-sheet + 3-XLSX + 1 MB IFC pack zips well under the 60 s budget). All four Phase 2 exit criteria, three of four Phase 5 criteria, and three of four Phase 6 criteria now flip from `[ ]` to `[x]`; the preview-latency criterion was originally backed by a Blender-IPC microbenchmark at the time of writing, and has since been superseded by the in-process `crates/aec_render/benches/native_render.rs` Criterion benchmark introduced in Phase 9 PR4 (which exercises the full native CPU/GPU path tracer + PBR rasterizer end to end).
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
