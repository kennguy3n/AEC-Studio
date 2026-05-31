# AEC Studio — Production Readiness Checklist

This document captures every invariant that must hold before AEC Studio
ships a public release. Each row links to the code that *currently
enforces* the invariant + the action the human release engineer must
take.

> **Maintenance contract:** if you change one of the enforcing code
> paths, update the corresponding row of this checklist in the same
> PR. The CI `production-readiness` gate (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml))
> runs the cross-cutting integration test
> [`crates/aec_bridge/tests/group_e_production_readiness.rs`](crates/aec_bridge/tests/group_e_production_readiness.rs)
> which pins these invariants at build time, but the checklist is the
> human-facing summary.

---

## 1 — Supply-chain & licensing

| # | Invariant | Enforced by | Release-engineer action |
|---|---|---|---|
| 1.1 | **No Python interpreter, modules, or `.py` files ship to end users.** | Workspace integration test [`crates/aec_ai/tests/no_python_invariant.rs`](crates/aec_ai/tests/no_python_invariant.rs); compile-time `ModelFormat` enum gate in [`crates/aec_ai/src/registry.rs`](crates/aec_ai/src/registry.rs); explicit per-crate audit in [`crates/aec_bridge/tests/group_e_production_readiness.rs`](crates/aec_bridge/tests/group_e_production_readiness.rs) (`no_python_in_aec_integrity_crate_source_tree`). | Confirm `cargo test --workspace --test no_python_invariant` is green in the release pipeline. Reject any new dep with `python*` / `pyo3` / `cpython` / `rustpython` in its transitive graph (`cargo tree -i pyo3 -- 2>&1 \| grep -c pyo3` must be zero). |
| 1.2 | **Every shipped GGUF model is pinned by BLAKE3 + size in `ai_models.json`.** | [`crates/aec_ai/data/ai_models.json`](crates/aec_ai/data/ai_models.json) (compile-time `include_str!` baked into the code-signed binary); registry validation in [`ModelRegistry::validate_at_boot`](crates/aec_ai/src/registry.rs). | When adding a new model: download → `blake3sum` → record exact hash + size → add to `ai_models.json` → verify `cargo test -p aec_ai --lib registry` passes. |
| 1.3 | **Outbound HTTP from the renderer is HTTPS-only**, with an explicit `http://localhost` / `http://127.0.0.1` exemption for local-dev sidecars. | URL scheme validation in [`BridgeService::image_gen_set_descriptor`](crates/aec_bridge/src/service.rs); allow-listed hosts in [`crates/aec_ai/src/model_download.rs`](crates/aec_ai/src/model_download.rs); per-call body cap in [`crates/aec_ai/src/http.rs`](crates/aec_ai/src/http.rs). | Confirm no `http://` URLs ship in `ai_models.json` (except `localhost` for sidecar URLs); validate the AllowedHosts list still covers exactly `huggingface.co` + project mirrors. |
| 1.4 | **AGPL compliance: source-code mirror reachable from any binary download.** | This file: link to `https://github.com/kennguy3n/AEC-Studio`. | Update [`README.md`](README.md) license section + corresponding entries in the Settings → About pane before each public release. |

## 2 — Binary integrity & trust chain

| # | Invariant | Enforced by | Release-engineer action |
|---|---|---|---|
| 2.1 | **Each platform binary is code-signed** (macOS Apple Developer ID notarized; Windows Authenticode; Linux signed by maintainer key). | [`apps/desktop/electron-builder.{yml,prod.json5}`](apps/desktop/) (CSC env vars) plus [`scripts/notarize/`](scripts/) hooks. | Provision Apple Developer ID + Windows codesign certs in CI secrets (`CSC_LINK`, `CSC_KEY_PASSWORD`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`). Manual smoke: `codesign --verify --deep --strict` on macOS bundle; `signtool verify /pa` on Windows MSI. |
| 2.2 | **`aec_integrity::TrustAnchor::production()` is empty (fail-closed)** until the signing server is provisioned. | [`crates/aec_integrity/src/signature.rs`](crates/aec_integrity/src/signature.rs); pinned by integration test `trust_anchor_production_is_empty_and_rejects_every_manifest` in [`crates/aec_bridge/tests/group_e_production_readiness.rs`](crates/aec_bridge/tests/group_e_production_readiness.rs). | When the signing server is online, replace `TrustAnchor::EMPTY` with `TrustAnchor::from_hex_keys(&[KEY1, KEY2, ...])` in a single commit + update this row. The integration test in Group E will then need an update to assert the production anchor key count matches the deployed signing-server fleet size (e.g. `assert_eq!(anchor.len(), 2)` for a primary + backup key). |
| 2.3 | **Every boot verifies every known model file against the compile-time-pinned BLAKE3 + size.** | [`BridgeService::model_integrity_report`](crates/aec_bridge/src/service.rs); inline regression tests in `service.rs::tests::model_integrity_report_*`; outer pin in [`crates/aec_bridge/tests/group_e_production_readiness.rs`](crates/aec_bridge/tests/group_e_production_readiness.rs). | No release-engineer action — the verification is automatic at every bridge boot. The desktop UI (Settings → Models) surfaces the report; the desktop QA pass should confirm a freshly-installed binary shows `verified` for every downloaded model and `missing` for everything else. |
| 2.4 | **Tamper / mismatch surfaces a clear diagnostic, never a silent crash or silent re-download.** | `ModelVerificationStatus::{Mismatch, SizeMismatch, ReadError}` variants in [`crates/aec_integrity/src/boot.rs`](crates/aec_integrity/src/boot.rs); UI flow in [`apps/desktop/renderer/src/components/AiModelsSection.tsx`](apps/desktop/renderer/src/components/AiModelsSection.tsx). | QA pass: with a release candidate, manually corrupt one byte of a downloaded model file → restart the app → confirm Settings → Models shows a *mismatch* warning and the affected sidecar refuses to spawn. |

## 3 — Sidecar lifecycle observability

| # | Invariant | Enforced by | Release-engineer action |
|---|---|---|---|
| 3.1 | **Every sidecar spawn / kill / timeout / exit emits a structured `tracing` event** tagged `sidecar = "ai"` or `sidecar = "image-gen"`. | [`AiState::ensure_ready`](crates/aec_bridge/src/ai_state.rs) + [`AiState::reload_with_config`](crates/aec_bridge/src/ai_state.rs); [`ImageGenState::ensure_ready`](crates/aec_bridge/src/image_gen_state.rs) + [`ImageGenState::reload_with_config`](crates/aec_bridge/src/image_gen_state.rs) + [`ImageGenState::maybe_unload`](crates/aec_bridge/src/image_gen_state.rs). | Confirm the desktop binary registers a `tracing-subscriber` writer to a rolling log file under the user-data dir (so support can ask the user for the log). The event taxonomy + field names are stable; any change must update [`docs/SIDECAR_TELEMETRY.md`](docs/SIDECAR_TELEMETRY.md) (TODO if missing). |
| 3.2 | **Cold-spawn elapsed time is captured in the success/failure event** so latency regressions are visible in operator logs. | `elapsed_ms` field on both `cold-spawn ready` (info) + `cold-spawn failed` (warn) events. | No action — pinned by the test suite. |
| 3.3 | **Idle-evict only logs when a child was actually shut down**, not on every governor tick. | `was_running` branch in [`ImageGenState::maybe_unload`](crates/aec_bridge/src/image_gen_state.rs); pinned by `maybe_unload_is_noop_until_idle_window_elapses_in_ready_state` unit test. | No action. |

## 4 — Concurrency & lock ordering

| # | Invariant | Enforced by | Release-engineer action |
|---|---|---|---|
| 4.1 | **`image_gen_generate` does not hold the process-wide SERVICE `RwLock` reader for the multi-minute sampling window.** | Prepare/run split in [`BridgeService::image_gen_prepare_generate`](crates/aec_bridge/src/service.rs) + [`run_image_gen_generate`](crates/aec_bridge/src/service.rs); napi handler at [`crates/aec_bridge/src/napi_api.rs`](crates/aec_bridge/src/napi_api.rs); regression test `image_gen_prepare_run_split_releases_service_lock_before_blocking_work` in `service.rs::tests`. | No action. Releases blocking on this hold any other bridge writer for up to ~4 min — would surface as the app freezing every concurrent operation. |
| 4.2 | **`ensure_ready` releases `handle_slot` and `restart_policy` before grabbing `runtime` (write).** | Documented lock-ordering invariant + scope-boundary comment at [`crates/aec_bridge/src/image_gen_state.rs:249-263`](crates/aec_bridge/src/image_gen_state.rs); regression test `ensure_ready_failure_path_releases_all_locks_in_canonical_order`. | No action. |

## 5 — Renderer / desktop surface

| # | Invariant | Enforced by | Release-engineer action |
|---|---|---|---|
| 5.1 | **`ImageGenPanel` rejects out-of-spec dimensions client-side** (multiple-of-8 widths/heights; clamp steps to `[1, 150]`; clamp `cfg_scale` to `[0.0, 30.0]`). | `normalizeImageGenDimension` / `clampImageGenSteps` / `clampImageGenCfg` in [`apps/desktop/renderer/src/components/ImageGenPanel.tsx`](apps/desktop/renderer/src/components/ImageGenPanel.tsx); server-side defense-in-depth in [`BridgeService::image_gen_generate`](crates/aec_bridge/src/service.rs). | No action. UI vs bridge validators are pinned in sync by both `vitest` + Rust test suites. |
| 5.2 | **Path-traced render gate on image-gen** disables Generate for Low/Medium tiers while a non-realtime render is in progress. | `gatedByRender` + render-queue poll in [`apps/desktop/renderer/src/components/ImageGenPanel.tsx`](apps/desktop/renderer/src/components/ImageGenPanel.tsx); server-side gate in [`BridgeService::image_gen_generate`](crates/aec_bridge/src/service.rs); poll-gating by policy at lines 288-294 (no IPC waste on High/Pro). | No action. |

## 6 — Release-pipeline procedure

Before tagging a release:

1. **Rebase + clean tree:** `git rebase origin/main` on the release branch; `git status` must be clean.
2. **Workspace test:** `cargo test --workspace --all-targets`. All Rust tests must pass.
3. **Workspace lints:** `cargo clippy --workspace --all-targets -- -D warnings` zero warnings, `cargo fmt --all --check` clean.
4. **Renderer test:** `pnpm vitest run` all green, `pnpm tsc --noEmit` clean.
5. **No-Python invariant test (explicit):** `cargo test --workspace --test no_python_invariant` — this is a smoke-test that the AGPL/no-Python invariant *and* the `ModelFormat::Gguf`-only serde gate hold for the to-be-shipped binary.
6. **Group E pin:** `cargo test -p aec_bridge --test group_e_production_readiness` — confirms all Tasks 23-26 invariants compose at the public bridge surface.
7. **Code-sign + notarize:** macOS via `electron-builder` + `notarize`; Windows via `signtool` + `electron-builder`. CSC env vars set in CI secrets.
8. **Smoke install:** install the release-candidate binary on a fresh user account (macOS / Windows / Linux); open the app cold; confirm:
   - Settings → Models shows every entry as `missing` (not `mismatch`).
   - Tampering a downloaded model file → restart → Settings → Models shows `mismatch` + the corresponding sidecar refuses to spawn.
   - Sidecar logs are written to the platform user-data dir + can be retrieved by support workflow.
9. **AGPL source-mirror smoke:** the Settings → About pane links to the public GitHub mirror; the link resolves.
10. **Sign release notes** with the platform-default GPG / SSH key (`git tag -s vX.Y.Z`).

---

## 7 — Out of scope for this checklist (future work)

The Phase 18 Group E commit landed the verification *scaffold*; the
following items require user-side infrastructure decisions and live
in a future release:

- Apple Developer ID procurement + macOS notarization workflow.
- Windows Authenticode certificate procurement + EV signing flow.
- ed25519 model-signing infrastructure (signing-server stand-up,
  key rotation policy, manifest distribution). Once provisioned,
  swap `TrustAnchor::production()` from `EMPTY` to the real key
  set as described in row 2.2.
- Reproducible builds — currently the binary is reproducible per
  `rustc` version + `cargo` lockfile pin, but not bit-for-bit
  reproducible across builders. Tracked as a long-tail security
  improvement.

When any of the above land, this checklist must be updated in the
same PR so the human release engineer's procedure stays accurate.
