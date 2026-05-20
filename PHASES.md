# Phases

High-level summary of the seven AEC Studio delivery phases. The
canonical, up-to-date status — including per-item check marks, exit
criteria, and changelog — lives in
[PROGRESS.md](PROGRESS.md). This file is intentionally short so it
stays readable as the project grows.

| Phase | Goal (one line) | Status |
|---|---|---|
| **0 — Repository & docs** | Initialize repo, AGPL-3.0 license, and the full documentation suite. | `DONE` |
| **1 — Technical validation** | Stand up the Rust workspace, Electron shell, native bridge, project package format, and AI sidecar runtime end-to-end. | `DONE` |
| **2 — ArchViz / Interior Studio MVP** | A solo interior designer can take an apartment from new project to client renders without leaving the app. | `IN PROGRESS` (build 100 %, exit-criteria validation pending) |
| **3 — 2D CAD module** | Drafters can produce construction documentation in pure 2D and roundtrip DXF cleanly. | `DONE` |
| **4 — BIM Lite / IFC** | Small studios can import IFC, classify, edit properties, generate schedules + BOQ-lite, and re-export with GUID preservation. | `DONE` |
| **5 — Render pipeline hardening** | Renders are reliable, reproducible, and fast enough to be part of the daily delivery workflow. | `IN PROGRESS | ~85 %` |
| **6 — Deliver and export** | A studio lead can ship a complete delivery package — client, contractor, BIM, revision-tracked — from one project. | `IN PROGRESS | ~85 %` |
| **7 — Optional KChat integration** | Teams using KChat can publish AEC Studio artifacts and route review comments back to the audit trail without giving up local-first. | `NOT STARTED` |

## Cross-cutting

| Item | Status |
|---|---|
| Linux desktop support (profiler, Blender discovery, packaging, CI) | `IN PROGRESS` — core + packaging configs + CI job landed; runtime soak pending |
| macOS desktop support | `IN PROGRESS` — covered by the Phase 1 + 2 work; packaging targets exist |
| Windows desktop support | `IN PROGRESS` — covered by the Phase 1 + 2 work; packaging targets exist |
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
