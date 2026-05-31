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

/// Process-wide image-gen runtime state. All methods take `&self`
/// because the two internal primitives provide the synchronisation
/// directly — [`crate::service::BridgeService`] can hold it by Arc
/// without an outer mutex.
pub struct ImageGenState {
    runtime: RwLock<ImageGenRuntime>,
    handle_slot: Mutex<Option<ImageGenHandle>>,
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
            if handle.try_exit_code().is_some() {
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
            match sidecar::spawn(&cfg.spawn_config, cfg.load_budget.min(spawn_timeout)) {
                Ok(handle) => {
                    let transport = handle.transport().clone();
                    *slot = Some(handle);
                    self.runtime.write().map_err(poisoned)?.mark_ready();
                    Ok(transport)
                }
                Err(e) => {
                    let msg = e.to_string();
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
        *slot = None;
        let mut runtime = self.runtime.write().map_err(poisoned)?;
        runtime.reload_with_config(new_config);
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
}
