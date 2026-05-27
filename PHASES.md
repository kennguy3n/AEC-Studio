# Phases

High-level summary of the AEC Studio delivery phases (currently 0–12). The
canonical, up-to-date status — including per-item check marks, exit
criteria, and changelog — lives in
[PROGRESS.md](PROGRESS.md). This file is intentionally short so it
stays readable as the project grows.

| Phase | Goal (one line) | Status |
|---|---|---|
| **0 — Repository & docs** | Initialize repo, AGPL-3.0 license, and the full documentation suite. | `DONE` |
| **1 — Technical validation** | Stand up the Rust workspace, Electron shell, native bridge, project package format, and AI sidecar runtime end-to-end. | `DONE` |
| **2 — ArchViz / Interior Studio MVP** | A solo interior designer can take an apartment from new project to client renders without leaving the app. | `DONE` |
| **3 — 2D CAD module** | Drafters can produce construction documentation in pure 2D and roundtrip DXF cleanly. | `DONE` |
| **4 — BIM Lite / IFC** | Small studios can import IFC, classify, edit properties, generate schedules + BOQ-lite, and re-export with GUID preservation. | `DONE` |
| **5 — Render pipeline hardening** | Renders are reliable, reproducible, and fast enough to be part of the daily delivery workflow. | `DONE` |
| **6 — Deliver and export** | A studio lead can ship a complete delivery package — client, contractor, BIM, revision-tracked — from one project. | `DONE` |
| **7 — Optional KChat integration** | Teams using KChat can publish AEC Studio artifacts and route review comments back to the audit trail without giving up local-first. | `DONE` |
| **8 — Extension system** | Asset packs, templates, schedules, export targets, AI tools, and importers ship as Ed25519-signed third-party extensions with a typed permission model. | `DONE` |
| **9 — Native render & BIM engine** | Replace the Blender and IfcOpenShell worker processes with in-process Rust implementations (SAH BVH, wgpu compute path tracer, PBR rasterizer preview, STEP parser/writer, geometry tessellator). No external runtime dependency for rendering or IFC. | `DONE` |
| **10 — N-API bridge completion** | Wire every `BridgeBackend` method through the N-API boundary — no more in-process fallbacks. Promote the 9 `draft.*` / `deliver.*` methods listed in `NATIVE_FALLBACK_METHODS` into the wired set so every gesture journals through `command_apply` (Immediate transaction, audit chain, undo-able). | `DONE` |
| **11 — Real domain depth** | Replace stubs and scaffolding with real implementations across the workspace: AI accept-diff → `command_apply`, template instantiation produces real walls/floors/ceilings/rooms, filesystem revision snapshots + BLAKE3 version diff, DXF round-trip fidelity, DWG bridge wiring, real PDF / SVG / glTF exports, asset import pipeline with LOD chain + thumbnails, real IFC schedules from project data, SQLite-backed render job persistence, incremental constraint solver, IES profile parsing, BLAKE3 audit-chain verification, per-OS thermal monitor + governor backoff, and end-to-end user-journey integration tests for the interior, drafter, and PM workflows. | `DONE` |
| **12 — Production depth, KChat local IPC, viewport pipeline** | Take every Phase 0–11 surface from "works in tests" to "ships to users". `LocalIpcTransport` + `LocalIpcPublisher` + `KChatDiscovery` connect to a real KChat Desktop instance over a UNIX socket / named pipe; the wgpu viewport ships a real adapter + `RenderPipeline` + `SurfaceManager` with frame coalescing; deliver packs carry real PNGs / XLSX / PDFs / IFC / SVG content from the project graph (no placeholder bytes); the AI sidecar adapter spawns a real `llama-server` with `/health` checks, GBNF-constrained `/completion`, idle unload, and crash restart; the final render pipeline + PBR preview + IES GPU texture + NLM denoiser + tile progress streaming light up the production render path; and a cross-cutting hardening pass adds `ProjectPackage::open` forward-only migrations with pre-migration backup, memory-pressure eviction in the governor, SQL-backed undo/redo journal, plus Phase 5 and Phase 7 end-to-end journey tests. | `DONE` |

## Cross-cutting

| Item | Status |
|---|---|
| Linux desktop support (profiler, packaging, CI, runtime soak) | `DONE` — `crates/aec_governor/tests/linux_soak.rs` covers GPU probe, tier classification, and scheduler admission |
| macOS desktop support | `DONE` — Phase 1 + 2 work plus `package-macos` CI job (`.dmg` + `.zip`, universal binary, hardened runtime) |
| Windows desktop support | `DONE` — Phase 1 + 2 work plus `package-windows` CI job (NSIS `.exe` + `.msi`, `.aec` file association) |
| Local-first storage (`.aecstudio` SQLCipher) | `DONE` |
| Audit chaining (BLAKE3) | `DONE` |
| Governor + tier classification | `DONE` |
| Local AI sidecar runtime | `DONE` |

## Where to look next

* For per-item check marks and the running changelog, see
  [PROGRESS.md](PROGRESS.md).
* For architecture and platform notes, see
  [ARCHITECTURE.md](ARCHITECTURE.md).
* For the original product proposal, see
  [PROPOSAL.md](PROPOSAL.md).
* For how to set up the toolchain and run tests, see
  [README.md](README.md#quick-start).
