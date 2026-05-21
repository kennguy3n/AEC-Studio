# License Architecture

> Detailed legal-engineering analysis of every license boundary in AEC Studio.
> This document is the source of truth for what AEC Studio is allowed to link
> against, ship with, and invoke at runtime, and how each open-source dependency
> stays compatible with AEC Studio's AGPL-3.0 license.

This document complements [PROPOSAL.md § "Licensing and compliance strategy"](../PROPOSAL.md)
and [README.md § "License & open-source posture"](../README.md). It records the
concrete legal reasoning behind each component choice — what we link, what we
fork, what we invoke as a process, and what users may swap.

---

## TL;DR

| Component | License | How AEC Studio uses it | Boundary | Compatible with AGPL-3.0? |
|---|---|---|---|---|
| AEC Studio (this repository) | AGPL-3.0 | The application itself | n/a | n/a |
| llama.cpp (PrismML fork) | MIT | Local AI sidecar (`llama-server`) | **Out-of-process only** — HTTP loopback | Yes (MIT is permissive) |
| wgpu | MIT OR Apache-2.0 | Native viewport renderer | Linked into Rust core | Yes (permissive) |
| napi-rs (`napi`, `napi-derive`, `napi-build`) | MIT | Electron ↔ Rust bridge | Linked into the bridge crate (cdylib) | Yes (permissive) |
| SQLCipher + OpenSSL | BSD-style (SQLCipher) + Apache-2.0 (OpenSSL ≥ 3.0) | Encrypted local project DB | Linked statically via `rusqlite` `bundled-sqlcipher-vendored-openssl` | Yes (permissive) |
| Electron | MIT | Desktop shell | Bundled binary | Yes (permissive) |
| MLX | MIT | Apple-Silicon AI acceleration adapter | Linked into the inference adapter | Yes (permissive) |
| `printpdf` | MIT | PDF generation for client deliverables | Linked into `aec_export` | Yes (permissive) |
| `glam` | MIT OR Apache-2.0 | Linear algebra for geometry and viewport | Linked into Rust crates | Yes (permissive) |
| `sysinfo` | MIT | Hardware profiler | Linked into `aec_governor` | Yes (permissive) |

The strict rule is: **anything copyleft stronger than LGPL must be reachable only across a process boundary**. As of Phase 9 (PRs #9–#13), AEC Studio has **zero** runtime dependencies stronger than MIT — the only remaining external process is the `llama-server` AI sidecar (MIT-licensed). Rendering and IFC parsing, which previously required GPL (Blender) and LGPL (IfcOpenShell) external processes, are now in-process Rust.

---

## Why AGPL-3.0 for AEC Studio

AEC Studio's license is **AGPL-3.0** because:

1. **The substrate repos (kennguy3n/knowledge, kennguy3n/Tessera) are MIT / Apache-2.0** —
   we reuse their patterns but the application as a whole is published under a
   stronger copyleft, since AEC Studio is the user-facing product and not a
   library.
2. **AGPL-3.0 preserves user freedoms even for hosted / SaaS deployments.**
   Earlier phases of AEC Studio invoked Blender (GPL-3.0) and IfcOpenShell
   (LGPL-3.0) as out-of-process workers; AGPL-3.0 was selected as the
   strictest copyleft that was still compatible with those boundaries.
   Phase 9 replaced both with native Rust, so the AGPL-3.0 choice is no
   longer driven by upstream GPL constraints — it is now driven purely by
   AEC Studio's own ethos: a local-first, open-source AEC tool whose source
   should remain available even if it is ever fronted by a network service.
3. **Section 13 (network use)** is intentional: a future remote-rendering /
   farm-mode would expose corresponding source automatically. This
   is a feature, not a bug, for a local-first open-source AEC tool.

---

## Blender (GPL-3.0): no longer used at runtime

**License of Blender:** [GNU GPL-3.0-or-later](https://www.blender.org/about/license/).

**Status:** Phase 9 PR4 removed every Blender dependency from AEC Studio.
The `workers/blender/` directory, `BlenderWorker`, `blender_discovery`, and
the Cycles / EEVEE wrappers (`cycles.rs`, `eevee.rs`) were deleted. AEC
Studio's runtime now contains **no Blender binary, no Blender Python scripts,
and no Blender process boundary** — rendering is performed in-process by
[`crates/aec_render/`](../crates/aec_render) using a native Rust path tracer
(SAH BVH + wgpu compute kernel with CPU fallback) and a native PBR
rasterizer for preview.

**Reference reading.** The path tracer architecture was informed by reading
[kennguy3n/cycles](https://github.com/kennguy3n/cycles) (specifically
`src/integrator/path_trace.cpp`, `src/kernel/bvh/traversal.h`,
`src/kernel/closure/bsdf_principled*.h`, and `src/integrator/render_scheduler.h`).
Reading a permissively-licensed (Apache-2.0) reference and writing an
independent clean-room implementation in Rust does **not** create a license
obligation: AEC Studio's `aec_render` crate is original code that compiles to
our own binary, not a translation or recompilation of Cycles. The GPL/AGPL
attaches to *conveyed* code, not to architectural ideas.

**Cycles (Apache-2.0).** No longer invoked. The native path tracer replaces
both EEVEE preview and Cycles final render in a single Rust crate.

---

## IfcOpenShell (LGPL-3.0): no longer used at runtime

**License of IfcOpenShell:** [GNU LGPL-3.0-or-later](https://github.com/IfcOpenShell/IfcOpenShell/blob/master/COPYING).

**Status:** Phase 9 PR5 removed every IfcOpenShell dependency from AEC
Studio. The `workers/ifc/` directory and the Python subprocess that wrapped
`ifcopenshell` were deleted. IFC parsing, writing, and geometry tessellation
are now in-process Rust:

- [`crates/aec_bim/src/ifc/reader.rs`](../crates/aec_bim/src/ifc/reader.rs) —
  ISO 10303-21 STEP parser with schema detection (IFC2x3 / IFC4 / IFC4x3),
  streaming iterator, and verbatim preservation of unknown measure types.
- [`crates/aec_bim/src/ifc/writer.rs`](../crates/aec_bim/src/ifc/writer.rs) —
  deterministic GUID-preserving STEP emitter, lossless round-trip of
  unmodeled property types via `PropertyValue::Other { measure, raw }`.
- [`crates/aec_bim/src/tessellator.rs`](../crates/aec_bim/src/tessellator.rs) —
  IfcExtrudedAreaSolid, IfcFacetedBrep, profile types
  (rectangle/circle/arbitrary closed), and basic CSG for
  IfcBooleanClippingResult.

**Reference reading.** Like Cycles, the native parser was informed by
reading [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell)
as a *reference for the STEP format and IFC entity model*, then implemented
from scratch in Rust. ISO 10303-21 and the IFC schema definitions
(buildingSMART) are public standards; an independent implementation against
those standards is not a derivative work of IfcOpenShell.

LGPL-3.0 is one-way compatible with AGPL-3.0 — we *could* have continued
linking IfcOpenShell as a library — but a native Rust implementation is
materially better for performance, crash isolation (Rust memory safety),
and cross-platform packaging (no Python runtime dependency).

---

## llama.cpp / PrismML fork (MIT): bundling-compatible

**License of llama.cpp:** [MIT License](https://github.com/ggerganov/llama.cpp/blob/master/LICENSE).
The PrismML fork inherits MIT (see [kennguy3n/llama.cpp](https://github.com/kennguy3n/llama.cpp)).

MIT is fully permissive and AGPL-compatible. AEC Studio invokes the
`llama-server` binary as a **subprocess** (`workers/ai/`) and talks to it via
**loopback HTTP** on `127.0.0.1`. We do not link `libllama` directly.

**Why subprocess, not library.** Even though MIT permits linking:

- The PrismML sidecar must be **hot-swappable** — users can drop in a
  different model or upgrade the runtime without rebuilding AEC Studio.
- The sidecar manages its own GPU/CPU acceleration backends (CUDA, Metal,
  Vulkan, AVX-VNNI). Embedding it in-process would dramatically increase the
  binary's surface area and platform-specific build complexity.
- Process isolation is consistent with the rest of the worker pattern.

---

## wgpu (MIT OR Apache-2.0)

`wgpu` is a permissive (MIT / Apache-2.0 dual-licensed) Rust crate. It is
linked into the `aec_viewport` crate and statically compiled into the N-API
addon. No license obligations beyond preserving the MIT / Apache notices.

---

## napi-rs (MIT)

`napi`, `napi-derive`, and `napi-build` are MIT-licensed. They are linked
into the `aec_bridge` cdylib. The output `.node` file is part of AEC Studio
and inherits AEC Studio's AGPL-3.0 license; the napi-rs runtime support code
linked into it is MIT and stays MIT.

---

## SQLCipher + OpenSSL (BSD-style + Apache-2.0)

`rusqlite` with `bundled-sqlcipher-vendored-openssl` builds both SQLCipher
and OpenSSL into the AEC Studio binary. Both are permissive licenses fully
compatible with AGPL-3.0:

- **SQLCipher:** BSD-style (open-source community edition).
- **OpenSSL ≥ 3.0:** Apache-2.0 (Apache 2.0 replaced the old dual OpenSSL +
  SSLeay licenses as of OpenSSL 3.0). The OpenSSL version vendored by
  `rusqlite` for SQLCipher is 3.x, so the historical Apache-2.0
  incompatibility with GPLv2 does not apply.

---

## Electron (MIT)

Electron is MIT-licensed. The Electron binary AEC Studio ships is unmodified.
Electron's bundled Chromium is BSD-style. No additional obligations.

---

## Bundled assets

Bundled furniture, materials, and textures are tagged with their own SPDX
identifier in `assets/<pack>/manifest.json`. The bundled licenses must be
one of:

- **CC0-1.0** (public domain)
- **CC-BY-4.0** (attribution preserved in asset manifests)
- **CC-BY-SA-4.0** (treated as compatible with AGPL because they are
  assets, not code; attribution and license inheritance are preserved in
  the asset manifest)
- **AGPL-3.0** assets contributed under the project's own license

Non-free or restrictively licensed assets (e.g. "personal use only",
"no AI training") are **not** bundled and must be installed by the user as
opt-in packs.

---

## Process boundary checklist

Every dependency review of AEC Studio must confirm:

1. No GPL/AGPL code is linked into a process containing
   permissive-only code without explicit license review.
2. No code with the following licenses is statically linked into the AEC
   Studio binary:
   - GPL-2.0, GPL-3.0
   - SSPL (MongoDB-style)
   - "Commons Clause" or similar source-available-only licenses
3. Each new crate added to `Cargo.toml`'s workspace must declare a
   permissive or AGPL-compatible license.

A CI lint that fails on disallowed SPDX identifiers (e.g. `cargo-deny`
with `licenses.deny = ["GPL-2.0", "GPL-3.0", "AGPL-3.0", "SSPL-1.0"]` for
the crates that link into the in-process binary) will be added in
Phase 1's CI work.

---

## References

- [GNU GPL-3.0](https://www.gnu.org/licenses/gpl-3.0.en.html)
- [GNU LGPL-3.0](https://www.gnu.org/licenses/lgpl-3.0.en.html)
- [GNU AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.en.html)
- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
- [GPL FAQ — Mere Aggregation](https://www.gnu.org/licenses/gpl-faq.en.html#MereAggregation)
- [PROPOSAL.md § Licensing and compliance strategy](../PROPOSAL.md)
- [ARCHITECTURE.md § Security and privacy](../ARCHITECTURE.md)
- [SECURITY.md](../SECURITY.md)
