# AEC Studio AI stack — architecture reference

> **Single source of truth** for the on-device AI subsystem shipped in
> AEC Studio v1 + Phase 18. Combines the text-inference stack
> ([Phase 9](../../PHASES.md), [`AI_RUNTIME.md`](../AI_RUNTIME.md))
> and the image-generation stack
> ([Phase 18 Groups A-F](../../PHASES.md)) into one reference document.
>
> This is the doc reviewers, ops, and future maintainers should read
> first. Implementation lives across `crates/aec_ai/`,
> `crates/aec_bridge/`, `crates/aec_governor/`, and
> `crates/aec_integrity/` — citations into the source tree are
> file-path-plus-line wherever the contract is load-bearing.

---

## 1. Stack at a glance

AEC Studio ships **two** AI sidecars, both following the same
architectural pattern: out-of-process native binary, HTTP loopback,
no Python, governor-policy-gated, cold-spawn on demand, idle-evicted.

| Sidecar | Binary | Models | Wire format | Crate |
|---|---|---|---|---|
| **Text** | [`llama-server`](https://github.com/kennguy3n/llama.cpp) (PrismML llama.cpp fork) | Ternary-Bonsai 1.7B / 4B / 8B — **1.58-bit GGUF (Q2_0)** | `llama.cpp` HTTP `/completion` | [`crates/aec_ai/src/`](../../crates/aec_ai/src/) (`runtime.rs`, `sidecar.rs`, `transport.rs`) |
| **Image-gen** | User-supplied native SD server (`sd-server`, `stable-diffusion.cpp`, custom A1111-shaped) | User-supplied **GGUF only** descriptor (see [`ai_models.json::image_gen`](../../crates/aec_ai/data/ai_models.json)) | A1111-shaped `/sdapi/v1/txt2img` | [`crates/aec_ai/src/image_gen/`](../../crates/aec_ai/src/image_gen/) (`runtime.rs`, `sidecar.rs`, `transport.rs`) |

**Both** sidecars share:

- The [governor policy contract](#5-governor-policies-per-hardware-tier) — per-tier load budget, idle timeout, parallelism, render co-occupation rules.
- The [no-Python ship invariant](#3-no-python-ship-invariant) — enforced by `crates/aec_ai/tests/no_python_invariant.rs` at every `cargo test`.
- The [BLAKE3 boot-time verification pipeline](#7-integrity-verification-flow) — provided by `crates/aec_integrity` and called from `BridgeService::model_integrity_report()`.
- The [structured tracing telemetry taxonomy](#6-telemetry-event-taxonomy) — `sidecar = "ai" | "image-gen"` tagged events for every spawn / kill / timeout / error.

The two sidecars are **independent** — text generation cannot block on
image generation (and vice versa) at the runtime layer. They share the
process-wide `BridgeService` `RwLock` only for the brief request-validation
phase; the multi-second / multi-minute cold-spawn + generate phases run
under the prepare/run split (see [§8](#8-bridgeservice-prepare-run-split)).

---

## 2. Crate map

```
crates/
├── aec_ai/                 native LLM + image-gen client code
│   ├── src/
│   │   ├── transport.rs            text sidecar HTTP wire (/completion)
│   │   ├── runtime.rs              text sidecar runtime state machine
│   │   ├── sidecar.rs              text llama-server spawn/kill discipline
│   │   ├── model_manager.rs        text-tier metadata (ModelTier → descriptor)
│   │   ├── registry.rs             ai_models.json loader + serde (rejects non-GGUF formats)
│   │   ├── model_download.rs       HF download, Range-resume, BLAKE3 verify
│   │   ├── http.rs                 shared loopback HTTP client (per-call body cap)
│   │   ├── image_gen/
│   │   │   ├── transport.rs        SD sidecar HTTP wire (/sdapi/v1/txt2img)
│   │   │   ├── runtime.rs          SD sidecar runtime state machine (mirror of text)
│   │   │   ├── sidecar.rs          SD spawn/kill + restart policy
│   │   │   ├── model_manager.rs    image-gen descriptor (user-supplied)
│   │   │   └── mod.rs              re-exports
│   │   └── lib.rs                  public API surface
│   ├── data/
│   │   └── ai_models.json          baked-in registry — three text tiers + empty image-gen presets
│   └── tests/
│       └── no_python_invariant.rs  ship-time invariant: zero .py outside the build-time allow-list
│
├── aec_bridge/             desktop bridge — bridges aec_ai + aec_governor to napi
│   ├── src/
│   │   ├── ai_state.rs             text sidecar lifecycle handle wrapper + telemetry
│   │   ├── image_gen_state.rs      image-gen sidecar lifecycle handle wrapper + telemetry
│   │   ├── service.rs              BridgeService: napi-facing methods + prepare/run split
│   │   └── napi_api.rs             napi-rs JS export surface
│   └── tests/
│       ├── group_e_production_readiness.rs   Task 27 outer integration test
│       └── group_f_e2e_lifecycle.rs          Task 28 end-to-end lifecycle test
│
├── aec_governor/           hardware-tier-keyed policy table
│   └── src/policy.rs               GovernorPolicy + AiPolicy + ImageGenPolicy + RenderPolicy
│
└── aec_integrity/          shared BLAKE3 + ed25519 verification primitives
    ├── src/
    │   ├── blake3_digest.rs        streaming BLAKE3 (64 KiB chunks)
    │   ├── boot.rs                 verify_files_against_pins → VerificationReport
    │   └── signature.rs            TrustAnchor + SignedManifest<P> (fail-closed default)
    └── tests/                      pinned: production() trust anchor is empty
```

The renderer surfaces this stack via three IPC namespaces in
`apps/desktop/electron/preload.ts`:

| Namespace | Backed by | Surface |
|---|---|---|
| `aec.ai` | `BridgeService::ai_*` | model availability, download, plan, runtime status, integrity report |
| `aec.imageGen` | `BridgeService::image_gen_*` | descriptor pin, download, generate, runtime status, apply policy |
| `aec.governor` | `BridgeService::governor_*` | apply hardware tier, fetch active policy |

---

## 3. No-Python ship invariant

AEC Studio is a single-binary desktop app whose runtime AI dependencies
are *only* native binaries plus `*.gguf` model files. **The shipped
runtime contains zero `*.py` files** and bundles no `python3` / `pip` /
`mlx` / `gemlite` / `HQQ` / `conda` interpreter or wheel.

This is enforced at **four** layers — three are documentation /
process, the fourth is a `cargo test`-time mechanical assertion:

1. **Source tree.** No `workers/{ai,blender,ifc,python}/` directory. No
   `requirements.txt` / `pyproject.toml`. The handful of `.py` files
   present are build-time tooling on the CI runner (e.g. `packaging/generate_icons.py`,
   `scripts/generate_template_previews.py`) which emit PNGs and are
   never invoked at app runtime.
2. **CI.** No `setup-python` step, no `pip install` step. The historical
   `python-workers` CI job was removed in Phase 9 PR4 / PR5 when Blender
   and IFC workers were rewritten in native Rust.
3. **Installers.** `packaging/{macos,windows,linux}/electron-builder.*.yml`
   bundle only `dist/`, `dist-electron/`, `package.json`,
   `node_modules/`. No Python interpreter, no `mlx` / `gemlite` / `HQQ`
   wheels.
4. **Mechanical assertion at `cargo test` time.** The integration test
   `crates/aec_ai/tests/no_python_invariant.rs` walks the workspace
   tree (skip-list: `node_modules`, `target`, `.git`, `dist`,
   `dist-electron`) and fails the build if it finds:
   - Any `.py` outside the build-time allow-list (the two PNG generators above).
   - Any `requirements.txt` / `pyproject.toml` / `Pipfile` / `poetry.lock`.
   - A `workers/{ai,blender,ifc,python}/` directory.
   - A Python dependency in any `Cargo.toml` (e.g. `pyo3`, `mlx-rs`).
   - A Python package in any `package.json` `dependencies` /
     `devDependencies` (e.g. `python-shell`, `mlx`, `@huggingface/transformers`).
   - A reference to `python` / `pip` / `mlx` / `gemlite` / `HQQ` in
     `packaging/*/electron-builder.*.yml`.

The serde layer on `ai_models.json` adds a fifth defense:
[`ModelFormat`](../../crates/aec_ai/src/registry.rs) is a closed enum
that rejects any value other than `"gguf"` at deserialization. A
future PR that tries to add `"format": "mlx"` to `ai_models.json`
fails `BridgeService::new()` at boot **and** fails the registry's
own unit tests.

> **PrismML and MLX.** PrismML publishes MLX-2bit checkpoints
> (`prism-ml/Ternary-Bonsai-*-mlx-2bit`) on Hugging Face. Those exist
> as **conversion sources** for the GGUF-Q2_0 files we ship metadata
> for — the AEC Studio runtime never opens an MLX file because MLX
> requires the Python `mlx` and `mlx-lm` packages.

---

## 4. Sidecar lifecycle state machine

Both sidecars implement the same state machine (text:
[`aec_ai::runtime::RuntimeState`](../../crates/aec_ai/src/runtime.rs);
image-gen:
[`aec_ai::image_gen::runtime::ImageGenRuntimeState`](../../crates/aec_ai/src/image_gen/runtime.rs)).

```
                            ┌──────────────────────────┐
                            │           Idle           │ ◀────── reload_with_config
                            │ (no child, no transport) │ ◀────── maybe_unload (idle window elapsed)
                            └────────────┬─────────────┘ ◀────── reset (from Failed)
                                         │
                                  ensure_ready()
                                         │
                                         ▼
                            ┌──────────────────────────┐
                            │         Loading          │
                            │  (spawn_with_retry in    │
                            │   flight; spawn_timeout  │
                            │   from governor policy)  │
                            └─────┬──────────────┬─────┘
                                  │              │
                          spawn fails       spawn succeeds + /health = ok
                                  │              │
                                  ▼              ▼
                            ┌─────────┐    ┌──────────────────────────┐
                            │ Failed  │    │          Ready           │
                            │ (sticky │    │  (child alive; transport │
                            │  msg)   │    │   serves requests)       │
                            └─────┬───┘    └─┬───────────────┬────────┘
                                  │          │               │
                                  │       generate()      generate() finishes
                                  │          │               │
                                  │          ▼               │
                                  │     ┌─────────────┐      │
                                  │     │ Generating  │ ─────┘
                                  │     │ (sd-server  │
                                  │     │  txt2img    │
                                  │     │  in flight) │
                                  │     └──────┬──────┘
                                  │            │
                                  │      generate() errors
                                  │            │
                                  │            ▼
                                  └─────── Failed
```

Key invariants pinned by tests:

- **`ensure_ready` from `Failed` resets to `Idle` before re-spawning.**
  Sticky-until-explicit-reset is *contract* at the `mark_failed` layer;
  the bridge-level `ensure_ready` is the explicit retry. The renderer
  surfaces the last-error string via `runtimeStatus` polling so the
  user sees the failure before the next call clears it.
- **Lock order: `handle_slot → runtime → restart_policy`.** Documented at
  the top of `image_gen_state.rs`; pinned by
  `ensure_ready_failure_path_releases_all_locks_in_canonical_order`.
  The `restart_policy` mutex is held across `sidecar::spawn_with_retry`
  (which `std::thread::sleep`s between attempts) — this is **safe**
  because `restart_policy` is the lowest-order lock and all access
  sites hold `handle_slot` first, serializing all callers.
- **Pre-flight crash check.** Every `ensure_ready` calls
  `handle.try_exit_code()` before reusing the transport. A non-`None`
  exit code means the child died between requests (OOM, segfault, GPU
  driver crash) and triggers a `tracing::warn!` event + handle drop +
  cold re-spawn.
- **`reload_with_config` kills the running child.** The next
  `ensure_ready` cold-spawns under the new config rather than reusing
  the stale handle.
- **`maybe_unload` is idempotent.** The renderer's idle-eviction tick
  may fire repeatedly; the second call observes `Idle` and returns
  `Ok(false)` without side effects.

---

## 5. Governor policies per hardware tier

The hardware tier ([`HardwareTier`](../../crates/aec_governor/src/policy.rs))
is set once at app boot from the host CPU / GPU profile and may be
overridden by the user in Settings. It drives **every** budget decision
in the AI stack.

### Text sidecar ([`AiPolicy`](../../crates/aec_governor/src/policy.rs))

| Tier | `model_tier` | `max_parallel_requests` | `max_context_tokens` |
|---|---|---|---|
| Low | Small (1.7B) | 1 | 4 096 |
| Medium | Small (1.7B) | 2 | 6 144 |
| High | Medium (4B) | 2 | 8 192 |
| Pro | Large (8B) | 3 | 16 384 |

### Image-gen sidecar ([`ImageGenPolicy`](../../crates/aec_governor/src/policy.rs))

| Tier | `idle_timeout_secs` | `load_budget_secs` (= spawn timeout) | `max_parallel_requests` | `allow_during_pathtraced_render` |
|---|---|---|---|---|
| Low | 60 | 45 | 1 | false |
| Medium | 120 | 60 | 1 | false |
| High | 180 | 75 | 2 | **true** |
| Pro | 300 | 90 | 3 | **true** |

#### Why the per-tier values

- **`idle_timeout_secs`** monotonically increases with tier. On Low we
  evict aggressively because the host can't keep multi-GiB of weights
  resident under memory pressure; on Pro we keep the sidecar ready so
  successive generates are snappy.
- **`load_budget_secs`** is the cold-spawn budget *and* the
  `spawn_timeout` value handed to `spawn_with_retry`. The bridge
  trusts the governor's per-tier table verbatim — there is **no**
  hardcoded ceiling at the bridge layer. The previous code clamped
  with `.min(DEFAULT_IMAGE_GEN_SPAWN_TIMEOUT)` (60 s) which silently
  demoted High (75 s) and Pro (90 s) back to 60 s and tripped
  `HealthTimeout` on Pro workstations loading SDXL-sized models. The
  fix [`e75bfb6`](https://github.com/kennguy3n/AEC-Studio/commits/e75bfb6)
  removed the clamp; the values you see in this table are the
  effective spawn timeouts.
- **`max_parallel_requests`** caps the number of concurrent in-flight
  generates. The sidecar itself is serial; queue depth >1 lets the
  renderer batch a small UI burst. Low and Medium cap at 1 because
  each request holds a multi-GiB Vulkan / Metal context.
- **`allow_during_pathtraced_render`** gates spawning the image-gen
  sidecar while a path-traced render is `Running` in the render
  queue. False on Low / Medium (cpu/gpu thrash → crash); true on High
  / Pro (workstation can absorb both). This mirrors
  `RenderPolicy::allow_background_ai_during_render` for the text
  sidecar.

`#[serde(default)]` is set on
[`GovernorPolicy::image_gen`](../../crates/aec_governor/src/policy.rs)
so a `GovernorPolicy` JSON written by a pre-Group-C binary
deserializes cleanly with Medium-tier defaults instead of failing the
whole decode. Pinned by
`governor_policy_deserializes_legacy_json_without_image_gen_field`.

---

## 6. Telemetry event taxonomy

All sidecar lifecycle events are emitted through the `tracing` crate
so a future `tracing-subscriber` consumer (file rotation, OpenTelemetry
exporter, etc.) can pick them up without a code change in either
sidecar.

| Event | Level | Sidecar | Fields | When |
|---|---|---|---|---|
| `text sidecar exited between requests; resetting runtime + re-spawning on next ensure_ready` | `warn` | `ai` | `exit_code` | `ensure_ready` pre-flight check sees a non-`None` exit code |
| `text sidecar cold-spawn starting` | `info` | `ai` | `spawn_timeout_secs` | `ensure_ready` enters the spawn branch |
| `text sidecar cold-spawn ready` | `info` | `ai` | `elapsed_ms` | Spawn succeeded + `/health` returned ok |
| `text sidecar cold-spawn failed` | `warn` | `ai` | `elapsed_ms`, `error` | Spawn or health probe errored out |
| `text sidecar killed by reload_with_config; next ensure_ready will cold-spawn against new config` | `info` | `ai` | (none) | `reload_with_config` dropped a running child |
| `image-gen sidecar exited between requests; resetting runtime + re-spawning on next ensure_ready` | `warn` | `image-gen` | `exit_code` | (mirror) |
| `image-gen sidecar cold-spawn starting` | `info` | `image-gen` | `load_budget_secs`, `spawn_timeout_secs`, `max_attempts` | (mirror) |
| `image-gen sidecar cold-spawn ready` | `info` | `image-gen` | `elapsed_ms` | (mirror) |
| `image-gen sidecar cold-spawn failed` | `warn` | `image-gen` | `elapsed_ms`, `error` | (mirror) |
| `image-gen sidecar killed by reload_with_config; ...` | `info` | `image-gen` | (none) | (mirror) |
| `image-gen sidecar idle-evicted by governor maybe_unload tick` | `info` | `image-gen` | (none) | `maybe_unload` returned `true` |

Every event is tagged with `sidecar = "ai" | "image-gen"` so
multi-sidecar correlation is one filter away. The schema is **stable**
for the v1 production-readiness contract — adding a new field is a
non-breaking change; renaming an existing field is breaking. Pinned
by [`Task 27 production readiness integration test`](../../crates/aec_bridge/tests/group_e_production_readiness.rs).

---

## 7. Integrity verification flow

Two layers, both implemented in [`crates/aec_integrity/`](../../crates/aec_integrity/):

### 7.1. BLAKE3 file hashing (always live)

- **Streaming**: 64 KiB chunks via `std::io::copy` into the hasher —
  see [`blake3_file`](../../crates/aec_integrity/src/blake3_digest.rs).
  GGUF model files can be 8+ GiB; reading them into memory is a
  hard OOM on any consumer hardware.
- **Pinned digests**: each shipped text model has a compile-time
  BLAKE3 hex constant in
  [`ModelTier::canonical_blake3_hex`](../../crates/aec_ai/src/model_manager.rs).
  The download pipeline (`crates/aec_ai/src/model_download.rs`)
  streams into a `.partial` file while updating the hasher; on EOF
  the result is compared and a mismatch deletes the `.partial` file
  before propagating the error.
- **Boot-time verification**:
  [`BridgeService::model_integrity_report()`](../../crates/aec_bridge/src/service.rs)
  walks every known model (text tiers + image-gen descriptor) and
  emits a [`VerificationReport`](../../crates/aec_integrity/src/boot.rs)
  with per-file `status` of `Verified` / `Missing` / `Mismatch` /
  `ReadError`. **`Missing ≠ tamper`** — a pristine boot where no
  model has been downloaded yet returns `Missing` for every entry,
  not `Mismatch`, and does **not** quarantine. Pinned by the Task 27
  smoke-test contract.

### 7.2. ed25519 signature verification (fail-closed scaffold)

- **`SignedManifest<P>`** ([`signature.rs`](../../crates/aec_integrity/src/signature.rs))
  carries a typed payload (any `Serialize`/`Deserialize`-implementing
  `P`) plus an ed25519 signature and a key hint. Currently
  unused at the data plane — the bridge does not verify any
  signed manifest end-to-end yet.
- **`TrustAnchor`** holds a ring of `VerifyingKey`s; `verify_manifest`
  walks the ring and returns `Ok` on the first key that authenticates,
  `Err(SignatureError::UntrustedKey)` otherwise.
- **`TrustAnchor::production()` returns `Self::EMPTY`** —
  *fail-closed by design*. Until the signing-server / Apple Developer
  ID / Windows codesign cert provisioning lands, every manifest is
  rejected. Pinned by both `signature.rs::tests` and the Task 27
  integration test
  ([`trust_anchor_production_is_empty_and_rejects_every_manifest`](../../crates/aec_bridge/tests/group_e_production_readiness.rs)).
- **Distribution model (planned).** When real keys land, the trust
  anchor will be populated with the AEC Studio release-signing key + a
  rotation-secondary key. Every download metadata manifest (server-side
  generated; includes the BLAKE3 digest and pinned size) will be
  validated against the trust anchor before the BLAKE3 verification
  runs. This converts integrity from "the binary we ship contains the
  expected pinned hash" to "the binary we shipped + the metadata the
  signing server vouched for both match." The Task 27 contract test
  exists specifically so a future PR that populates
  `TrustAnchor::production()` **must** also update the test (which is
  what forces the new fixture data to land at the same time as the
  pubkey).

> See [`PRODUCTION_CHECKLIST.md`](../../PRODUCTION_CHECKLIST.md) row 2.2
> for the full sign-off matrix on integrity infrastructure.

---

## 8. BridgeService prepare/run split

The naive layout would acquire the process-wide `SERVICE` `RwLock`
read guard for the entire duration of a generate call — for image-gen
that's up to **240 seconds** (60 s cold spawn + 180 s sampling).
During this window every `BridgeService` writer (e.g. `command_apply`,
`project_save`) is blocked.

Group D added the prepare/run split:

```rust
// In napi_api.rs (the JS-facing handler):
let ctx = with_service_ref_fallible(|svc| svc.image_gen_prepare_generate(req))?;
// ↑ Brief reader guard (microseconds): validate request, snapshot
//   policy, clone Arc<ImageGenState>, compute spawn_timeout.
let result = BridgeService::run_image_gen_generate(ctx)?;
// ↑ NO bridge lock held. spawn_with_retry + transport.generate
//   run unattached so writers can proceed concurrently.
```

This pattern is used for **every long-running napi handler**:

| Napi handler | `prepare_*` (lock held) | `run_*` (no lock) |
|---|---|---|
| `ai_download_model` | [`ai_prepare_download`](../../crates/aec_bridge/src/service.rs) | [`run_ai_download`](../../crates/aec_bridge/src/service.rs) |
| `image_gen_download_model` | [`image_gen_prepare_download`](../../crates/aec_bridge/src/service.rs) | [`run_image_gen_download`](../../crates/aec_bridge/src/service.rs) |
| `image_gen_generate` | [`image_gen_prepare_generate`](../../crates/aec_bridge/src/service.rs) | [`run_image_gen_generate`](../../crates/aec_bridge/src/service.rs) |

The remaining `ai_plan` handler does **not** split because text
inference durations are ~30 s and would require a TOCTOU-aware
re-snapshot of the policy gate. This is a tracked follow-up
(see PROGRESS.md).

---

## 9. Registry pinning + download flow

### Text models — registry-pinned, downloaded by the bridge

The registry [`ai_models.json`](../../crates/aec_ai/data/ai_models.json) is **baked into the binary** at compile time via `include_str!` in
[`crates/aec_ai/src/registry.rs`](../../crates/aec_ai/src/registry.rs).
Each text tier carries:

| Field | Source of truth |
|---|---|
| `download_url` | Hugging Face URL on `huggingface.co` / `cdn-lfs.huggingface.co` (allow-listed in [`model_download.rs`](../../crates/aec_ai/src/model_download.rs)) |
| `filename` | Canonical file name on disk (e.g. `Ternary-Bonsai-1.7B-Q2_0.gguf`) |
| `size_bytes` | Authoritative byte count (rejection threshold during streaming download) |
| `blake3` | 64-char hex digest (verified incrementally during download + on every boot) |
| `format` | `"gguf"` (the serde layer rejects anything else) |

Download flow (text):

1. **Allow-list check.** Only `huggingface.co` /
   `cdn-lfs.huggingface.co`. `Location` headers on 30x are re-parsed
   and re-checked.
2. **`Range` resume.** A `.partial` file from a previous attempt is
   continued via `Range: bytes=<offset>-`; the server's
   `Content-Range` is parsed and compared against the local offset.
3. **Streaming write + incremental BLAKE3.** No in-memory buffering.
4. **Verification.** On EOF the streaming digest is compared to the
   pinned hash. Mismatch → delete `.partial`, propagate error.
5. **Atomic rename.** Successful download renames `.partial` → final
   name. Atomic on NTFS, APFS, ext4, btrfs, ZFS.

### Image-gen models — user-supplied descriptor

[`ai_models.json::image_gen.presets`](../../crates/aec_ai/data/ai_models.json)
intentionally ships as `[]`. A preset is only added once the
download → BLAKE3 → byte-count chain has been verified end-to-end by
the AEC Studio team. **Until that happens** the renderer's wizard
([`ImageGenPanel.tsx`](../../apps/desktop/renderer/src/components/ImageGenPanel.tsx))
shows the empty-state CTA and the user manually pins a descriptor
via the side-loaded path:

1. **Renderer → bridge**: `aec.imageGen.setDescriptor({ filename,
   download_url, size_bytes, blake3 })`. Validated by
   [`image_gen_set_descriptor`](../../crates/aec_bridge/src/service.rs):
   - `filename` non-empty, no path separators.
   - `download_url` must start with `https://` — `http://` is
     **rejected** with an explicit TLS-only-outbound-posture error
     (the downstream `model_download` host allow-list would catch any
     malformed URL anyway, but the descriptor-level check is a
     fast-fail for the most common mistake).
   - `size_bytes != 0` (an empty file is never a valid model).
   - `blake3` is exactly 64 lowercase hex chars.
2. **Renderer → bridge**: `aec.imageGen.downloadModel()`. Runs the
   same `model_download` engine as text models, with the user-supplied
   descriptor as the source of truth. The downloaded file is streamed,
   verified against the pinned BLAKE3, and atomically renamed.
3. **Renderer → bridge**: `aec.imageGen.generate({ prompt, width,
   height, steps, cfg_scale, negative_prompt, seed })`. The
   prepare/run split (see [§8](#8-bridgeservice-prepare-run-split))
   does the spawn + transport call. Width / height are validated as
   multiples of 8; steps as positive integer; `cfg_scale` as `f32`
   in `[0.0, 30.0]`.

The **side-loaded UX** is the explicit design choice for v1. When the
team blesses an SD GGUF as a preset, the wizard will auto-render it
as a one-click pin button; the side-loaded path remains for advanced
users / custom fine-tunes.

---

## 10. Loopback-only, no outbound during inference

During `ai_plan` / `image_gen_generate` the only socket open from the
AEC Studio process tree is the loopback connection to the sidecar
on `127.0.0.1`. Neither sidecar binary opens an outbound socket:
`llama-server` does not phone home, and the SD sidecars we recommend
(`sd-server`, `stable-diffusion.cpp`) also do not. The **only** egress
traffic from the entire AI subsystem is the model download itself
(Hugging Face HTTPS), which is gated behind the explicit "Download"
button in Settings and the host allow-list in `model_download.rs`.

Privacy guarantees on the download path:

- Fixed `User-Agent: AEC-Studio/<crate-version>` — no OS / browser /
  locale info.
- No cookies. `Set-Cookie` headers are ignored.
- No telemetry / analytics callbacks. The host allow-list enforces
  this.
- Default `rustls` certificate verifier. We do not disable cert
  checking, do not pin certificates (would force a release on every
  CA rotation), and do not accept self-signed certs.

---

## 11. End-to-end lifecycle test

[`crates/aec_bridge/tests/group_f_e2e_lifecycle.rs`](../../crates/aec_bridge/tests/group_f_e2e_lifecycle.rs)
exercises the **full happy path** through the public `BridgeService`
surface — the same `&BridgeService` reference the napi shim hands to
JS:

```
descriptor pin → download verify → spawn → generate → idle unload → re-spawn
```

The test runs against a mock TCP `/sdapi/v1/txt2img` server (canned
A1111-shaped responses) injected via `ImageGenState::__test_with_transport`,
so it does not require a real `sd-server` binary on the developer's
machine and runs deterministically in CI. State transitions
(`Idle` → `Loading` → `Ready` → `Generating` → `Ready` → eviction →
`Idle` → re-spawn → `Ready`) are asserted at every boundary.

Pair this with the other integration tests that pin specific layers:

- [`group_e_production_readiness.rs`](../../crates/aec_bridge/tests/group_e_production_readiness.rs)
  — boot smoke, model_integrity_report JSON shape, BLAKE3 streaming,
  TrustAnchor::production() empty-anchor rejection, verify_files
  Verified→Mismatch on tamper, no-Python in `aec_integrity`.
- [`no_python_invariant.rs`](../../crates/aec_ai/tests/no_python_invariant.rs)
  — workspace-wide no-Python invariant (`.py`, manifests, Cargo deps,
  npm deps, electron-builder configs).
- [`sidecar_mock.rs`](../../crates/aec_ai/tests/sidecar_mock.rs) — text
  sidecar happy path against a mock loopback HTTP server.
- [`phase2_e2e.rs`](../../crates/aec_ai/tests/phase2_e2e.rs) — text
  sidecar tool-planner dispatch end-to-end.

---

## 12. Future work

Tracked in [`PROGRESS.md`](../../PROGRESS.md) and
[`PRODUCTION_CHECKLIST.md`](../../PRODUCTION_CHECKLIST.md):

- **Populate `TrustAnchor::production()`** with the AEC Studio release
  signing key. The Task 27 test will start passing manifests through
  `verify_manifest()` once the fixture lands.
- **First blessed image-gen preset.** End-to-end verification of an SD
  GGUF (likely SDXL or FLUX-schnell) so the wizard exits the
  empty-state CTA branch.
- **`ai_plan` prepare/run split.** Currently text inference holds the
  bridge reader guard for the duration of the plan call (~30 s). A
  TOCTOU-aware policy re-snapshot needs to land first.
- **Telemetry consumer.** Hook a `tracing-subscriber` (likely
  `tracing-appender` with daily rotation) to a user-data log directory
  so support engineers can pull events without a debug build.
- **OpenTelemetry export.** Once the local file sink is stable, add an
  opt-in OTel exporter for org-managed deployments.
