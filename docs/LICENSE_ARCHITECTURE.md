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
| Blender + EEVEE | GPL-3.0 / GPL-2.0+ | Render worker | **Out-of-process only** — JSON-line IPC over stdin/stdout | Yes (process isolation; no linking) |
| IfcOpenShell | LGPL-3.0 | BIM import / export worker | **Out-of-process only** — user-swappable binary, JSON-line IPC | Yes (LGPL is one-way compatible with AGPL; we further isolate by process) |
| Cycles (kennguy3n fork) | Apache-2.0 | Final render path inside Blender worker | Invoked via Blender process | Yes (Apache-2.0 is permissive) |
| llama.cpp (PrismML fork) | MIT | Local AI sidecar (`llama-server`) | **Out-of-process only** — HTTP loopback | Yes (MIT is permissive) |
| wgpu | MIT OR Apache-2.0 | Native viewport renderer | Linked into Rust core | Yes (permissive) |
| napi-rs (`napi`, `napi-derive`, `napi-build`) | MIT | Electron ↔ Rust bridge | Linked into the bridge crate (cdylib) | Yes (permissive) |
| SQLCipher + OpenSSL | BSD-style (SQLCipher) + Apache-2.0 (OpenSSL ≥ 3.0) | Encrypted local project DB | Linked statically via `rusqlite` `bundled-sqlcipher-vendored-openssl` | Yes (permissive) |
| Electron | MIT | Desktop shell | Bundled binary | Yes (permissive) |
| MLX | MIT | Apple-Silicon AI acceleration adapter | Linked into the inference adapter | Yes (permissive) |
| `printpdf` | MIT | PDF generation for client deliverables | Linked into `aec_export` | Yes (permissive) |
| `glam` | MIT OR Apache-2.0 | Linear algebra for geometry and viewport | Linked into Rust crates | Yes (permissive) |
| `sysinfo` | MIT | Hardware profiler | Linked into `aec_governor` | Yes (permissive) |

The strict rule is: **anything copyleft stronger than LGPL must be reachable only across a process boundary**.

---

## Why AGPL-3.0 for AEC Studio

AEC Studio's license is **AGPL-3.0** because:

1. **The substrate repos (kennguy3n/knowledge, kennguy3n/Tessera) are MIT / Apache-2.0** —
   we reuse their patterns but the application as a whole is published under a
   stronger copyleft, since AEC Studio is the user-facing product and not a
   library.
2. **AGPL-3.0 is the strictest copyleft we can adopt that is still compatible
   with the GPL-3.0 dependencies we link via process isolation** (Blender). The
   GPL-3.0 ↔ AGPL-3.0 compatibility (GPL-3.0 §13 + AGPL-3.0 §13) means a
   user-bundled distribution of AEC Studio and a separately licensed Blender
   binary can coexist, **as long as Blender is never linked into AEC Studio's
   process space**.
3. **Section 13 (network use)** is intentional: a future remote-rendering /
   farm-mode (Phase 9+) would expose corresponding source automatically. This
   is a feature, not a bug, for a local-first open-source AEC tool.

---

## Blender (GPL-3.0): out-of-process worker only

**License of Blender:** [GNU GPL-3.0-or-later](https://www.blender.org/about/license/).

**Boundary:** Blender is **invoked as an external worker process**. AEC Studio's
Rust core (`aec_render` crate) spawns Blender (`blender --background --python …`)
as a subprocess and communicates only via:

- **stdin / stdout JSON lines** (see `workers/blender/aec_blender_worker.py`).
- **Read/write of project-scoped files** the user has already opted into.

There is **no dynamic linking, no static linking, no `dlopen`, no in-process
embedding**, and no shared address space between AEC Studio and Blender.

**Why this matters legally.** The GPL is triggered by *conveying combined
works in the same program*. Two separately compiled programs communicating
over a documented IPC channel are *aggregations*, not combined works (see GPL
FAQ: ["What is the difference between an 'aggregate' and other kinds of
'modified versions'?"](https://www.gnu.org/licenses/gpl-faq.en.html#MereAggregation)).
AEC Studio's process model is intentionally an aggregation, not a combination:

- Blender is shipped as a separate binary or downloaded by the user.
- Blender's GPL obligations apply to *Blender's* sources and any modified
  Blender binary the user redistributes — not to AEC Studio.
- The Blender worker scripts (`workers/blender/*.py`) **are themselves
  GPL-3.0** because they are Blender Python scripts that import the Blender
  Python API at runtime. They are clearly marked with an SPDX header and a
  `# SPDX-License-Identifier: GPL-3.0-or-later` at the top.

**User swap.** Users may replace the Blender binary on their machine (any
version in the supported range listed in `workers/blender/manifest.json`)
without rebuilding AEC Studio. This preserves the GPL's "Installation
Information" requirement on user products (GPL-3.0 §6) by keeping Blender
externally swappable.

**Cycles (Apache-2.0).** Cycles is invoked through Blender; AEC Studio never
links Cycles directly. Even if Cycles were re-licensed permissively, AEC
Studio still keeps the same process isolation to avoid future re-license
risk.

---

## IfcOpenShell (LGPL-3.0): out-of-process worker, user-swappable

**License of IfcOpenShell:** [GNU LGPL-3.0-or-later](https://github.com/IfcOpenShell/IfcOpenShell/blob/master/COPYING).

**Boundary:** IfcOpenShell is shipped as part of the BIM worker
(`workers/ifc/aec_ifc_worker.py`) and runs as a Python subprocess of AEC
Studio. The Python entrypoint imports `ifcopenshell` only inside that
subprocess.

**Why out-of-process even though LGPL is compatible.** LGPL-3.0 is one-way
compatible with AGPL-3.0; we could technically link IfcOpenShell directly
into the Rust core via a C FFI wrapper. We deliberately do not:

- **Defensive posture against accidental relicensing.** Out-of-process
  invocation insulates AEC Studio from any future license change to
  IfcOpenShell.
- **User-swappable binary requirement** (LGPL-3.0 §4 / §6). Users must be
  able to replace IfcOpenShell with a modified version. Process isolation
  is the cleanest way to make this exercisable.
- **Crash isolation.** A malformed IFC file should not be able to crash AEC
  Studio's Rust core.

**Worker scripts.** `workers/ifc/*.py` are licensed **LGPL-3.0** to match the
IfcOpenShell distribution. They carry an `SPDX-License-Identifier:
LGPL-3.0-or-later` header.

---

## Cycles (Apache-2.0): bundling-compatible

**License of Cycles:** [Apache License 2.0](https://github.com/blender/cycles/blob/main/COPYING)
(the kennguy3n fork preserves this license).

Apache-2.0 is a permissive license fully compatible with AGPL-3.0. AEC Studio
**does not link Cycles directly** — Cycles is invoked through Blender, which
provides Cycles' standard Python and command-line surface. The Apache-2.0
license therefore applies only to the user-installed Cycles binary; it
imposes no obligations on AEC Studio's sources.

If a future release of AEC Studio invokes a Cycles standalone binary
(without Blender), the same process-boundary rule applies: spawn it as a
subprocess, never `dlopen` it.

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
   - GPL-2.0, GPL-3.0 (Blender)
   - SSPL (MongoDB-style)
   - "Commons Clause" or similar source-available-only licenses
3. Worker scripts that import GPL Python APIs (e.g. Blender's `bpy`,
   IfcOpenShell) carry their own SPDX header matching the library's
   license, not AEC Studio's.
4. Each new crate added to `Cargo.toml`'s workspace must declare a
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
