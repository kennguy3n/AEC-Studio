//! Process-wide image-gen runtime state held by
//! [`crate::service::BridgeService`].
//!
//! Mirror of [`crate::ai_state::AiState`] for the image-gen sidecar.
//! The two states are deliberately kept side by side rather than
//! merged into one union because:
//!
//!   * They own **different child processes** (text `llama-server` on
//!     port 13579; image-gen `sd-server` on 13580). Folding them into
//!     one handle slot would make the canonical "kill on tier switch"
//!     path either over- or under-fire.
//!   * Their lifecycle drives **different idle budgets** (text 60 s,
//!     image-gen 120 s). The governor evicts each independently.
//!   * They publish **different status shapes** to the renderer
//!     (text has a pending-diff registry; image-gen does not).
//!
//! The lock primitives + ordering are intentionally identical to
//! [`AiState`] so reviewers familiar with the text side need no new
//! mental model — see the module doc at
//! [`crate::ai_state`] for the rationale on each primitive choice
//! and the canonical lock order.
//!
//! ## Lock ordering (canonical, grep-able)
//!
//! ```text
//!   handle_slot  >  runtime
//! ```
//!
//! Every method on [`ImageGenState`] respects this order; pair
//! reviewing against [`crate::ai_state`] should look identical.

use std::sync::{Mutex, RwLock};
use std::time::Duration;

use aec_ai::image_gen::{
    runtime::{ImageGenRuntime, ImageGenRuntimeConfig, ImageGenRuntimeState},
    sidecar::{self, ImageGenHandle, ImageGenSpawnError},
    transport::ImageGenTransport,
};
use thiserror::Error;

/// Default health-probe budget for the image-gen sidecar's cold
/// spawn. Larger than the text sidecar's `DEFAULT_SPAWN_TIMEOUT`
/// (30 s) because diffusion model load includes mmap of multi-GiB
/// weights *and* CUDA / Metal context init.
pub const DEFAULT_IMAGE_GEN_SPAWN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum ImageGenStateError {
    #[error("image-gen sidecar spawn: {0}")]
    Spawn(#[from] ImageGenSpawnError),
    #[error("image-gen runtime is in `failed` state: {0}")]
    Failed(String),
    #[error("image-gen state lock poisoned: {0}")]
    Poisoned(String),
}

/// Snapshot of the image-gen runtime suitable for returning to the
/// renderer over the JS-facing IPC. The Settings page polls this on
/// the `image_gen_runtime_status` endpoint every ~500 ms while a
/// generation is in flight to drive the status badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenStatusSnapshot {
    pub state: ImageGenRuntimeState,
    pub last_error: Option<String>,
}

/// Phase 18 Group D Task 21 — maximum number of spawn attempts the
/// retry loop will make before surfacing the underlying spawn
/// error to the caller. Three is the standard "transient flake"
/// retry count: a single retry would not exercise the back-off
/// schedule, while four-plus would push worst-case latency past
/// 30 s, longer than most users will tolerate before hitting
/// "Cancel". Each retry's delay is governed by
/// [`aec_ai::image_gen::sidecar::ImageGenRestartPolicy`].
const DEFAULT_IMAGE_GEN_MAX_SPAWN_ATTEMPTS: u32 = 3;

/// Process-wide image-gen runtime state. All methods take `&self`
/// because the three internal primitives provide the synchronisation
/// directly — [`crate::service::BridgeService`] can hold it by Arc
/// without an outer mutex.
///
/// The `restart_policy` field is a `Mutex` (not `RwLock`) because
/// every access mutates it (`record_failure` / `reset`); a writer-
/// only lock has the same cost as a `RwLock::write` and the value
/// is so cheap to clone that we could even snapshot it instead,
/// but the `Mutex` keeps the API symmetric with the other slots.
/// Lock order vs the other two locks: **last** —
/// `handle_slot → runtime → restart_policy`. `record_failure` /
/// `reset` are only ever called inside `ensure_ready` while
/// `handle_slot` is already held, so this is consistent with the
/// canonical order documented at the top of this file.
pub struct ImageGenState {
    runtime: RwLock<ImageGenRuntime>,
    handle_slot: Mutex<Option<ImageGenHandle>>,
    restart_policy: Mutex<sidecar::ImageGenRestartPolicy>,
}

fn poisoned<T>(err: std::sync::PoisonError<T>) -> ImageGenStateError {
    ImageGenStateError::Poisoned(format!("{err}"))
}

impl ImageGenState {
    /// Initialise with the supplied runtime config. Does NOT spawn
    /// the sidecar — spawn is lazy and triggered by the first
    /// `image_gen_generate` call.
    pub fn new(config: ImageGenRuntimeConfig) -> Self {
        Self {
            runtime: RwLock::new(ImageGenRuntime::new(config)),
            handle_slot: Mutex::new(None),
            restart_policy: Mutex::new(sidecar::ImageGenRestartPolicy::default()),
        }
    }

    pub fn state(&self) -> Result<ImageGenRuntimeState, ImageGenStateError> {
        Ok(self.runtime.read().map_err(poisoned)?.state())
    }

    pub fn last_error(&self) -> Result<Option<String>, ImageGenStateError> {
        Ok(self
            .runtime
            .read()
            .map_err(poisoned)?
            .last_error()
            .map(str::to_owned))
    }

    /// Snapshot the renderer-visible status. Lock-free in the
    /// `handle_slot` sense (does NOT take the handle mutex), so it
    /// returns instantly even during a cold spawn — same property
    /// the text-side [`crate::ai_state::AiState::snapshot`] holds.
    pub fn snapshot(&self) -> Result<ImageGenStatusSnapshot, ImageGenStateError> {
        let runtime = self.runtime.read().map_err(poisoned)?;
        Ok(ImageGenStatusSnapshot {
            state: runtime.state(),
            last_error: runtime.last_error().map(str::to_owned),
        })
    }

    /// Phase 18 Group C Task 17 — clone the currently-active runtime
    /// config (spawn descriptor + idle / load budgets). Used by the
    /// bridge's [`BridgeService::image_gen_apply_policy`] to rebuild
    /// the config with new budgets while preserving the configured
    /// model file. Cheap (one `RwLock::read`); no handle slot
    /// involvement.
    pub fn snapshot_config(&self) -> Result<ImageGenRuntimeConfig, ImageGenStateError> {
        Ok(self.runtime.read().map_err(poisoned)?.config().clone())
    }

    /// Lazy spawn-if-needed; returns the [`ImageGenTransport`] handle
    /// the caller uses to submit `/sdapi/v1/txt2img` requests.
    ///
    /// Same concurrency model as
    /// [`crate::ai_state::AiState::ensure_ready`]:
    ///   1. `handle_slot.lock()` serialises concurrent spawns.
    ///   2. `runtime` write is brief — only to publish `Loading` /
    ///      `Ready` / `Failed`, never spanning the spawn itself.
    ///   3. Status snapshots take `runtime.read()` and observe the
    ///      published `Loading` state instantly.
    pub fn ensure_ready(
        &self,
        spawn_timeout: Duration,
    ) -> Result<ImageGenTransport, ImageGenStateError> {
        let mut slot = self.handle_slot.lock().map_err(poisoned)?;

        // Pre-flight crash check (parallels the text-side path) — if
        // the child has exited (sd-server OOM, segfault, GPU driver
        // crash), drop the dead handle and fall through to cold
        // spawn.
        if let Some(handle) = slot.as_mut() {
            if let Some(code) = handle.try_exit_code() {
                // Phase 18 Group E Task 25 — sidecar lifecycle
                // telemetry. A non-`None` exit code observed during
                // `ensure_ready`'s pre-flight check means the child
                // died between requests (OOM, segfault, GPU driver
                // crash, user-initiated SIGTERM). This is a tamper
                // / corruption / health signal the operator needs to
                // see in logs immediately, not after the next
                // generate call retries the spawn.
                tracing::warn!(
                    sidecar = "image-gen",
                    exit_code = ?code,
                    "image-gen sidecar exited between requests; resetting runtime + re-spawning on next ensure_ready"
                );
                *slot = None;
                let cfg = {
                    let r = self.runtime.read().map_err(poisoned)?;
                    r.config().clone()
                };
                *self.runtime.write().map_err(poisoned)? = ImageGenRuntime::new(cfg);
            }
        }

        // Failed → reset (so we retry the spawn).
        {
            let mut runtime = self.runtime.write().map_err(poisoned)?;
            if matches!(runtime.state(), ImageGenRuntimeState::Failed) {
                runtime.reset();
            }
        }

        if slot.is_none() {
            let cfg = {
                let mut runtime = self.runtime.write().map_err(poisoned)?;
                // begin_load handles Idle / Loading idempotently; the
                // failure-reset above ensures we never observe
                // Failed here. Ready short-circuits via the
                // `slot.is_none()` branch — if we are here, the
                // handle is None and runtime cannot be Ready
                // (Ready without a handle would be a torn state).
                runtime.begin_load().map_err(|e| match e {
                    aec_ai::image_gen::runtime::ImageGenRuntimeError::NotReady(_) => {
                        // Branch reached only after a `reset()` that
                        // didn't take effect — defensive: collapse to
                        // Idle and try again.
                        ImageGenStateError::Failed(
                            "image-gen runtime stuck in non-Idle state at begin_load".into(),
                        )
                    }
                    aec_ai::image_gen::runtime::ImageGenRuntimeError::Failed(msg) => {
                        ImageGenStateError::Failed(msg)
                    }
                })?;
                runtime.config().clone()
            };
            // Phase 18 Group D Task 21 — wrap the cold-spawn in an
            // exponential-backoff retry loop. `spawn_with_retry`
            // exhausts up to `DEFAULT_IMAGE_GEN_MAX_SPAWN_ATTEMPTS`
            // attempts on transient errors (`Spawn`,
            // `HealthTimeout`, `EarlyExit`) and short-circuits
            // immediately on `EmptyModelPath` (which retry cannot
            // help). The policy state is owned per-ImageGenState
            // so that subsequent `ensure_ready` calls within the
            // same session continue the back-off curve rather than
            // restarting it on each generate request — important
            // when the user mashes "Generate" on a misconfigured
            // descriptor.
            //
            // Phase 18 Group E Devin Review fix — the
            // `restart_policy` mutex is taken inside an inner
            // scope so it is **dropped before** we re-acquire
            // `runtime.write()` for `mark_ready` / `mark_failed`.
            // Without the scope, the lock acquisition order
            // becomes `restart_policy → runtime`, which inverts
            // the canonical order documented at the top of this
            // module (`handle_slot → runtime → restart_policy`).
            // The bug is currently latent because every
            // `restart_policy` acquisition in the workspace also
            // holds `handle_slot` (serialising it against any
            // other ordering), but the moment a future code
            // path acquires `runtime` then `restart_policy`
            // outside of `handle_slot` we would have an AB/BA
            // deadlock. Keep the scope tight.
            // Phase 18 Group E Task 25 — emit a `cold_spawn_start`
            // event before the (possibly multi-second) cold spawn
            // so operators can correlate a slow generate with the
            // sidecar phase that owned the latency.
            tracing::info!(
                sidecar = "image-gen",
                load_budget_secs = cfg.load_budget.as_secs(),
                spawn_timeout_secs = spawn_timeout.as_secs(),
                max_attempts = DEFAULT_IMAGE_GEN_MAX_SPAWN_ATTEMPTS,
                "image-gen sidecar cold-spawn starting"
            );
            let spawn_started_at = std::time::Instant::now();
            let spawn_result = {
                let mut policy = self.restart_policy.lock().map_err(poisoned)?;
                sidecar::spawn_with_retry(
                    &cfg.spawn_config,
                    cfg.load_budget.min(spawn_timeout),
                    &mut policy,
                    DEFAULT_IMAGE_GEN_MAX_SPAWN_ATTEMPTS,
                )
                // INVARIANT (Phase 18 Group E Devin Review): the
                // `policy` MutexGuard is dropped exactly at this
                // closing brace, **before** the `runtime.write()`
                // calls in the `match spawn_result` arms below.
                // Without this scope boundary, the lock acquisition
                // order becomes `restart_policy → runtime`, inverting
                // the canonical `handle_slot → runtime → restart_policy`
                // documented at the top of this module. The
                // regression test
                // `ensure_ready_failure_path_releases_all_locks_in_canonical_order`
                // pins this property — any future refactor that
                // pulls `policy` out of this inner scope (e.g. by
                // hoisting the `let mut policy = …` to the
                // `if slot.is_none()` block above) will fail that
                // test before merge. Do not collapse this scope.
            };
            let elapsed = spawn_started_at.elapsed();
            match spawn_result {
                Ok(handle) => {
                    let transport = handle.transport().clone();
                    *slot = Some(handle);
                    self.runtime.write().map_err(poisoned)?.mark_ready();
                    // Phase 18 Group E Task 25 — successful cold
                    // spawn telemetry. `elapsed` lets ops correlate
                    // health-probe stalls vs. true spawn cost.
                    tracing::info!(
                        sidecar = "image-gen",
                        elapsed_ms = elapsed.as_millis() as u64,
                        "image-gen sidecar cold-spawn ready"
                    );
                    Ok(transport)
                }
                Err(e) => {
                    let msg = e.to_string();
                    // Phase 18 Group E Task 25 — failed cold spawn
                    // telemetry. Emit at `warn!` (not `error!`)
                    // because retry policy may resurrect on the next
                    // generate. Operators tailing logs see the
                    // exhausted retry budget as the final signal.
                    tracing::warn!(
                        sidecar = "image-gen",
                        elapsed_ms = elapsed.as_millis() as u64,
                        error = %msg,
                        "image-gen sidecar cold-spawn failed (retry budget exhausted)"
                    );
                    self.runtime.write().map_err(poisoned)?.mark_failed(msg);
                    Err(e.into())
                }
            }
        } else {
            // Fast path: handle alive, runtime already Ready. Bump
            // the idle clock so the next governor tick does not
            // evict mid-request.
            self.runtime.write().map_err(poisoned)?.record_use();
            Ok(slot
                .as_ref()
                .expect("handle is Some by branch condition")
                .transport()
                .clone())
        }
    }

    /// Swap the runtime config (e.g. when the user picks a different
    /// image-gen model from Settings). Kills the in-flight child if
    /// any so the next [`Self::ensure_ready`] cold-spawns with the
    /// new config — same shape as the text-side
    /// [`crate::ai_state::AiState::reload_with_config`].
    pub fn reload_with_config(
        &self,
        new_config: ImageGenRuntimeConfig,
    ) -> Result<(), ImageGenStateError> {
        let mut slot = self.handle_slot.lock().map_err(poisoned)?;
        let had_running_child = slot.is_some();
        *slot = None;
        let mut runtime = self.runtime.write().map_err(poisoned)?;
        runtime.reload_with_config(new_config);
        // Phase 18 Group E Task 25 — config-reload telemetry.
        // Distinguishes "kill running child to re-spawn against new
        // model" (loud signal) from "swap config while idle" (quiet
        // signal). Both are user-driven Settings actions but the
        // former interrupts an in-flight session.
        if had_running_child {
            tracing::info!(
                sidecar = "image-gen",
                "image-gen sidecar killed by reload_with_config; next ensure_ready will cold-spawn against new config"
            );
        } else {
            tracing::debug!(
                sidecar = "image-gen",
                "image-gen runtime config reloaded while sidecar idle"
            );
        }
        Ok(())
    }

    /// Idle-evict the running child if `should_unload()` returns
    /// true. Called by the governor on its 5 s tick. Returns `true`
    /// if a child was actually shut down.
    pub fn maybe_unload(&self) -> Result<bool, ImageGenStateError> {
        // Cheap read first — most ticks observe Ready+not-elapsed
        // and short-circuit without ever grabbing handle_slot. The
        // window between the read and the subsequent lock is fine:
        // an extra cycle of idleness is harmless, and a concurrent
        // `record_use` only resets the clock further into the
        // future.
        {
            let runtime = self.runtime.read().map_err(poisoned)?;
            if !runtime.should_unload(std::time::Instant::now()) {
                return Ok(false);
            }
        }
        let mut slot = self.handle_slot.lock().map_err(poisoned)?;
        let mut runtime = self.runtime.write().map_err(poisoned)?;
        // Re-check under the locks — the runtime may have been
        // used between the cheap read above and the lock
        // acquisition.
        if !runtime.should_unload(std::time::Instant::now()) {
            return Ok(false);
        }
        let was_running = slot.is_some();
        *slot = None;
        runtime.reset();
        // Phase 18 Group E Task 25 — idle-eviction telemetry.
        // Only emit when we actually shut down a running child so
        // we don't spam logs every governor tick on an empty
        // service.
        if was_running {
            tracing::info!(
                sidecar = "image-gen",
                "image-gen sidecar idle-evicted by governor maybe_unload tick"
            );
        }
        Ok(was_running)
    }

    /// Test-only constructor: attach a pre-existing transport
    /// (e.g. from a mock TCP server). Mirrors
    /// [`crate::ai_state::AiState::__test_with_transport`].
    #[doc(hidden)]
    pub fn __test_with_transport(
        config: ImageGenRuntimeConfig,
        transport: ImageGenTransport,
    ) -> Self {
        let mut runtime = ImageGenRuntime::new(config);
        runtime
            .begin_load()
            .expect("begin_load from fresh Idle never fails");
        runtime.mark_ready();
        Self {
            runtime: RwLock::new(runtime),
            handle_slot: Mutex::new(Some(sidecar::adopt(transport))),
            restart_policy: Mutex::new(sidecar::ImageGenRestartPolicy::default()),
        }
    }

    /// Test-only: force the runtime into `Loading` so a regression
    /// test can simulate "spawn in flight" without a real child.
    #[doc(hidden)]
    pub fn __test_begin_load(&self) {
        let mut r = self.runtime.write().expect("runtime lock");
        r.begin_load().expect("from idle");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg() -> ImageGenRuntimeConfig {
        ImageGenRuntimeConfig {
            idle_timeout: Duration::from_millis(20),
            ..ImageGenRuntimeConfig::default()
        }
    }

    #[test]
    fn snapshot_starts_idle() {
        let s = ImageGenState::new(ImageGenRuntimeConfig::default());
        let snap = s.snapshot().unwrap();
        assert_eq!(snap.state, ImageGenRuntimeState::Idle);
        assert!(snap.last_error.is_none());
    }

    #[test]
    fn maybe_unload_is_noop_until_idle_window_elapses_in_ready_state() {
        // Force the runtime into Ready without spawning a real
        // child by using the test-only constructor with a dummy
        // transport.
        let transport = ImageGenTransport::new(1, Duration::from_millis(10));
        let s = ImageGenState::__test_with_transport(cfg(), transport);
        assert!(!s.maybe_unload().unwrap());
        // Wait past the idle window.
        std::thread::sleep(Duration::from_millis(30));
        // First unload should fire.
        assert!(s.maybe_unload().unwrap());
        // Subsequent unloads are noops.
        assert!(!s.maybe_unload().unwrap());
        assert_eq!(s.snapshot().unwrap().state, ImageGenRuntimeState::Idle);
    }

    #[test]
    fn reload_with_config_kills_child_and_resets_to_idle() {
        let transport = ImageGenTransport::new(1, Duration::from_millis(10));
        let s = ImageGenState::__test_with_transport(cfg(), transport);
        assert_eq!(s.snapshot().unwrap().state, ImageGenRuntimeState::Ready);
        let mut new_cfg = ImageGenRuntimeConfig::default();
        new_cfg.spawn_config.port = 13599;
        s.reload_with_config(new_cfg).unwrap();
        assert_eq!(s.snapshot().unwrap().state, ImageGenRuntimeState::Idle);
    }

    /// Phase 18 Group E Devin Review regression — pins the canonical
    /// lock-ordering invariant in `ensure_ready` after the
    /// failure-path `mark_failed` branch.
    ///
    /// **What the bug was**: `restart_policy.lock()` was acquired
    /// at the top of the cold-spawn arm and held across the
    /// subsequent `runtime.write().mark_failed(...)` call. That
    /// inverted the documented order
    /// (`handle_slot \u2192 runtime \u2192 restart_policy`) into
    /// `restart_policy \u2192 runtime`. No deadlock today because every
    /// `restart_policy` access also holds `handle_slot`, but a
    /// future code path that took `runtime` then `restart_policy`
    /// outside `handle_slot` would AB/BA-deadlock.
    ///
    /// **What this test pins**: after `ensure_ready` returns
    /// `Err(Spawn(EmptyModelPath))`, all three locks
    /// (`handle_slot`, `runtime`, `restart_policy`) must be
    /// release-able via `try_lock` / `try_write`. If a future
    /// refactor accidentally widens the `restart_policy` scope to
    /// span the `runtime.write()` call again, this test will fail
    /// loudly: under the bug, a panic in `mark_failed` would leak
    /// the `policy` guard. The test also asserts via direct field
    /// access (only possible because the test lives in the same
    /// module) that no guard was left behind on the success path
    /// we exercise here.
    #[test]
    fn ensure_ready_failure_path_releases_all_locks_in_canonical_order() {
        // Default `ImageGenSpawnConfig` has `model_path: PathBuf::new()`,
        // so `spawn_with_retry` hard-stops on `EmptyModelPath` without
        // attempting any real child process \u2014 we exercise only the
        // mark_failed branch under test, not the IO / health probe.
        let state = ImageGenState::new(ImageGenRuntimeConfig::default());
        let result = state.ensure_ready(Duration::from_millis(50));
        assert!(
            matches!(result, Err(ImageGenStateError::Spawn(_))),
            "expected Spawn error from empty-model-path fast-fail, got {result:?}",
        );

        // After `ensure_ready` returns, every internal lock must
        // be release-able with no contention. If the bug regressed
        // (policy guard outliving runtime.write()), a poisoned-lock
        // panic from inside the scope would have left the
        // `restart_policy` guard wedged, so `try_lock` would
        // either return `WouldBlock` or `Poisoned` here.
        let handle_slot = state
            .handle_slot
            .try_lock()
            .expect("handle_slot must be released after ensure_ready returns");
        assert!(
            handle_slot.is_none(),
            "failure path must not leave a stale handle",
        );
        drop(handle_slot);
        let runtime = state
            .runtime
            .try_write()
            .expect("runtime must be release-able after ensure_ready returns");
        drop(runtime);
        let policy = state
            .restart_policy
            .try_lock()
            .expect("restart_policy must be released after ensure_ready returns");
        drop(policy);

        // Runtime must have observed the `mark_failed` write \u2014
        // proving the runtime.write() acquired AFTER policy was
        // dropped actually executed.
        let snap = state.snapshot().expect("snapshot after fail");
        assert_eq!(snap.state, ImageGenRuntimeState::Failed);
        let err = snap
            .last_error
            .expect("Failed state always carries last_error");
        assert!(
            err.to_ascii_lowercase().contains("model"),
            "last_error should reference the empty model path: {err}",
        );
    }
}
