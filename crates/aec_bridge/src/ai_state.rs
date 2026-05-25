//! Process-wide AI runtime state held by [`crate::service::BridgeService`].
//!
//! Owns three pieces of state, each behind its **own** synchronisation
//! primitive so that the JS-facing AI surface stays responsive even
//! while a cold-spawn is in flight:
//!
//! 1. A [`SidecarRuntime`] state machine (Idle / Loading / Ready / Failed)
//!    behind an [`RwLock`]. The state is mutated briefly during
//!    [`AiState::ensure_ready`] (`begin_load` → `mark_ready` /
//!    `mark_failed`) and read by [`AiState::snapshot`] on every
//!    `ai_runtime_status` poll. The renderer polls status every ~500 ms
//!    while a plan is in flight, so this is the contended read path —
//!    [`RwLock`] lets every poll proceed concurrently with every other
//!    poll, and only briefly blocks during a state transition.
//!
//! 2. A `Mutex<Option<SidecarHandle>>` — the spawned `llama-server`
//!    child process. The [`Mutex`] serialises concurrent
//!    [`AiState::ensure_ready`] callers (only one cold-spawn at a
//!    time) and is also taken by [`AiState::cancel_job`] to terminate
//!    the running child. During a cold-spawn this [`Mutex`] IS held
//!    for the full `spawn_timeout` window, but the only methods that
//!    touch it are `ensure_ready` and `cancel_job`. Status polls go
//!    through the lock-free `runtime` `RwLock` and observe the
//!    published `Loading` state instantly.
//!
//! 3. A `Mutex<HashMap<DiffId, PendingDiff>>` of pending diffs. The
//!    renderer accepts / rejects diffs by id; this mutex is
//!    uncontended in practice (the renderer serialises its own user
//!    interactions client-side) and is fully independent of sidecar
//!    lifecycle. Each entry pairs the diff with the project path it
//!    targets so a later `ai_accept_diff` knows which project to
//!    apply against — see Phase 11 task 10 in PROGRESS.md.
//!
//! ## Why three primitives instead of one
//!
//! The earlier design held all three pieces behind a single
//! `Mutex<AiState>`. That meant the **first** `ai_plan` of a session
//! — which has to spawn the sidecar and wait up to `spawn_timeout`
//! (30 s by default) for its `/health` probe — held the mutex for
//! the entire spawn window, and every concurrent `ai_runtime_status`
//! / `ai_cancel_job` / `ai_accept_diff` call blocked on it. Combined
//! with `#[napi]` sync functions running on the libuv main thread,
//! that froze the Electron UI for the full 30 s.
//!
//! Splitting along the *natural* concurrency boundaries (lifecycle
//! state is read by polling, diff registry is independent, spawn
//! exclusivity is the only thing that needs serialisation) lets:
//!
//! - `ai_runtime_status` polls return in O(microseconds) even during
//!   a cold-spawn (they take the `runtime` `RwLock` *read* side, and
//!   `Loading` is already published).
//! - `ai_accept_diff` / `ai_reject_diff` execute concurrently with
//!   a cold-spawn (they only touch `pending_diffs`).
//! - `ai_cancel_job` still serialises against `ensure_ready` (they
//!   share the `handle_slot` mutex), which is correct: terminating
//!   the handle while the spawn is racing to populate it would be a
//!   use-after-free shaped bug.
//!
//! Together with the napi-layer `spawn_blocking_napi` wrapping (see
//! `crate::napi_api` module doc on the AI endpoints), this means the
//! libuv main thread is never blocked by AI work, *and* the
//! renderer's status pane keeps refreshing during cold-spawn.
//!
//! ## Lock ordering (canonical, grep-able)
//!
//! When any single code path needs to hold two (or all three) of the
//! primitives simultaneously, it acquires them in this strictly
//! increasing order:
//!
//! ```text
//!   handle_slot  >  runtime  >  pending_diffs
//! ```
//!
//! Every method on [`AiState`] respects this order, which means a
//! circular-wait deadlock is statically impossible:
//!
//! | Method                | handle_slot | runtime               | pending_diffs |
//! |-----------------------|-------------|-----------------------|---------------|
//! | `ensure_ready`        | take (lock) | brief write (×1-4)    | —             |
//! | `cancel_job`          | take (lock) | brief write           | —             |
//! | `snapshot`            | —           | read                  | lock          |
//! | `state` / `last_error`| —           | read                  | —             |
//! | `insert_diff`         | —           | —                     | lock          |
//! | `accept_diff` / `reject_diff` | —   | —                     | lock          |
//!
//! Notably, [`AiState::snapshot`] — the renderer's hot read path —
//! deliberately does **not** touch `handle_slot`. That is what lets
//! the cold-spawn status-poll responsiveness contract hold: while
//! `ensure_ready` is parked on `/health` holding `handle_slot`, every
//! concurrent `snapshot()` call takes only the cheap pair
//! (`runtime.read()` → `pending_diffs.lock()`) and returns instantly.
//! The integration test
//! `ai_runtime_status_returns_loading_instantly_during_cold_spawn`
//! in `crates/aec_bridge/tests/ai_endpoints.rs` pins this property —
//! a regression that adds `handle_slot` to `snapshot()` would block
//! the test for the full simulated spawn window and fail by an order
//! of magnitude.
//!
//! When adding new methods to [`AiState`], stay within this order. If
//! you find yourself needing a different ordering, the right fix is
//! to extend the snapshot/registry boundary (e.g. publish more state
//! into `SidecarRuntime` so it can be read without taking `handle_slot`),
//! NOT to reorder the locks.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use aec_ai::{
    sidecar::{self, SidecarHandle, SidecarSpawnError},
    Diff, RuntimeConfig, RuntimeState, SidecarRuntime,
};
use aec_core::types::DiffId;
use thiserror::Error;

/// Default health-probe budget when the bridge lazily spawns the sidecar.
/// Cold-cache model load on a laptop SSD is ~5 s; 30 s gives the user
/// enough headroom on a slow machine without leaving the renderer
/// hanging on a fundamentally broken setup.
pub const DEFAULT_SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum AiStateError {
    #[error("ai sidecar spawn: {0}")]
    Spawn(#[from] SidecarSpawnError),
    #[error("ai runtime is in `failed` state: {0}")]
    Failed(String),
    #[error("ai diff `{0}` not found")]
    UnknownDiff(String),
    #[error("ai plan: {0}")]
    Plan(#[from] aec_ai::PlanError),
    #[error("ai state lock poisoned: {0}")]
    Poisoned(String),
}

/// Snapshot of `[AiState`] suitable for returning to the renderer.
/// Carrying it as an owned struct (rather than a `&AiState`) frees the
/// caller to drop any internal guards before serialising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiStatusSnapshot {
    pub state: RuntimeState,
    pub last_error: Option<String>,
    /// Currently-tracked pending diff ids — useful for the renderer's
    /// "AI sidebar" to enumerate proposals on reconnect.
    pub pending_diff_ids: Vec<String>,
}

/// Process-wide AI state. All methods take `&self` because the three
/// internal primitives provide the synchronisation directly. This
/// means [`crate::service::BridgeService`] can hold `AiState` by
/// value rather than wrapping it in an outer `Mutex`, and every AI
/// endpoint runs without serialising against unrelated AI traffic.
pub struct AiState {
    /// Lifecycle state machine + last_error. See module doc.
    runtime: RwLock<SidecarRuntime>,
    /// Spawn slot — `None` until the first `ai_plan` call. Dropping
    /// the [`SidecarHandle`] kills the `llama-server` child.
    handle_slot: Mutex<Option<SidecarHandle>>,
    /// Diff registry; independent of sidecar lifecycle. Each entry
    /// holds the project path so [`crate::service::BridgeService::ai_accept_diff`]
    /// can re-open the right encrypted package and route the
    /// converted commands through the project's own command engine.
    pending_diffs: Mutex<HashMap<DiffId, PendingDiff>>,
}

/// A diff registered by `ai_plan`, waiting for the renderer to call
/// `ai_accept_diff` or `ai_reject_diff`. Bundles the [`Diff`] with
/// the project path AND the scope it was planned against so the
/// accept path can apply the resulting commands to the right project
/// graph even if the renderer has since switched the active project
/// (and so the AI audit log records the *planned* scope rather than
/// the renderer's current view). The project path + scope are
/// captured at insertion time \u2014 binding them to the diff (rather
/// than reading the active project / scope at accept time) is what
/// makes "plan on A in Design, switch to B in Draft, accept the A
/// diff" deterministic.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingDiff {
    pub project_path: String,
    pub scope: aec_core::types::Scope,
    pub diff: Diff,
}

/// Convenience: turn a `PoisonError<T>` (which is not `Send + 'static`
/// in a generic way) into our owned [`AiStateError::Poisoned`].
fn poisoned<T>(err: std::sync::PoisonError<T>) -> AiStateError {
    AiStateError::Poisoned(format!("{err}"))
}

impl AiState {
    /// Initialise with the sidecar config baked from
    /// `workers/ai/config.json`. Does NOT spawn the sidecar yet —
    /// spawning is deferred until the first `ai_plan` call so the
    /// renderer can boot without paying the model-load cost.
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            runtime: RwLock::new(SidecarRuntime::new(config)),
            handle_slot: Mutex::new(None),
            pending_diffs: Mutex::new(HashMap::new()),
        }
    }

    /// Current lifecycle state. Lock-free (RwLock read).
    pub fn state(&self) -> Result<RuntimeState, AiStateError> {
        Ok(self.runtime.read().map_err(poisoned)?.state())
    }

    /// Last failure message, if the runtime is in `Failed`. Lock-free
    /// (RwLock read).
    pub fn last_error(&self) -> Result<Option<String>, AiStateError> {
        Ok(self
            .runtime
            .read()
            .map_err(poisoned)?
            .last_error()
            .map(str::to_owned))
    }

    /// Snapshot the renderer-visible state in one call. Takes the
    /// `runtime` read lock and the `pending_diffs` mutex; both are
    /// released before returning. **Critically, this method does NOT
    /// touch `handle_slot`**, so it returns instantly even during a
    /// cold-spawn — the spawn-side `Loading` write completes before
    /// the spawn blocks on `/health`, and every read after that sees
    /// the published `Loading` state.
    pub fn snapshot(&self) -> Result<AiStatusSnapshot, AiStateError> {
        let runtime = self.runtime.read().map_err(poisoned)?;
        let diffs = self.pending_diffs.lock().map_err(poisoned)?;
        Ok(AiStatusSnapshot {
            state: runtime.state(),
            last_error: runtime.last_error().map(str::to_owned),
            pending_diff_ids: diffs.keys().map(|id| id.as_str().to_owned()).collect(),
        })
    }

    /// Lazy spawn-if-needed; returns the transport descriptor. The
    /// first call pays the cold-cache cost (`spawn_timeout`);
    /// subsequent calls see a `Ready` runtime and short-circuit.
    ///
    /// On spawn failure the runtime transitions to `Failed` and the
    /// error is propagated; the next call retries the spawn.
    ///
    /// Concurrency model:
    ///
    /// 1. The `handle_slot` mutex is acquired up-front. This is the
    ///    point at which concurrent `ensure_ready` callers serialise
    ///    — exactly one of them does the spawn work; the others
    ///    block here, observe `handle.is_some()`, and short-circuit.
    /// 2. The `runtime` `RwLock` is grabbed in *write* mode only
    ///    briefly: to reset a `Failed` runtime, to publish
    ///    `Loading`, and to publish `Ready` / `Failed` after the
    ///    spawn. These writes are O(microseconds); they do NOT
    ///    span the spawn itself.
    /// 3. Between the `Loading` write and the `Ready` write, the
    ///    `runtime` lock is **not** held, so concurrent
    ///    `ai_runtime_status` polls take the read side and observe
    ///    `Loading` instantly.
    pub fn ensure_ready(
        &self,
        spawn_timeout: Duration,
    ) -> Result<aec_ai::SidecarTransport, AiStateError> {
        let mut slot = self.handle_slot.lock().map_err(poisoned)?;

        // Pre-flight crash check: if we have a handle but the child
        // has exited (OOM / segfault / host `pkill`), drop the dead
        // handle and fall through to the spawn path so the next
        // `complete` call doesn't fail with a confusing connection-
        // refused error. `try_exit_code` is non-blocking
        // (`Child::try_wait`).
        if let Some(handle) = slot.as_mut() {
            if handle.try_exit_code().is_some() {
                *slot = None;
                // Reset runtime so the upcoming `begin_load` doesn't
                // surface a stale `Failed` from a previous crash.
                let cfg = {
                    let r = self.runtime.read().map_err(poisoned)?;
                    r.config().clone()
                };
                *self.runtime.write().map_err(poisoned)? = SidecarRuntime::new(cfg);
            }
        }

        // Failed → reset (so a subsequent ensure_ready attempts to
        // spawn again). Done as a brief write under `runtime`.
        {
            let mut runtime = self.runtime.write().map_err(poisoned)?;
            if matches!(runtime.state(), RuntimeState::Failed) {
                let cfg = runtime.config().clone();
                *runtime = SidecarRuntime::new(cfg);
            }
        }

        if slot.is_none() {
            // Publish `Loading` BEFORE blocking on the spawn so
            // concurrent status polls see it. Drop the write guard
            // immediately after the transition so any spawn-side
            // panic doesn't leave the lock held.
            let cfg = {
                let mut runtime = self.runtime.write().map_err(poisoned)?;
                runtime.begin_load();
                runtime.config().clone()
            };
            match sidecar::spawn(&cfg, spawn_timeout) {
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
            // Fast path: handle alive, runtime already `Ready`. Just
            // bump the `last_used` timestamp.
            self.runtime.write().map_err(poisoned)?.record_use();
            Ok(slot
                .as_ref()
                .expect("handle is Some by branch condition")
                .transport()
                .clone())
        }
    }

    /// Test-only constructor: attach a pre-existing transport (e.g.
    /// from a mock TCP server) without spawning a child process. The
    /// runtime is moved straight to `Ready`.
    #[doc(hidden)]
    pub fn __test_with_transport(
        config: RuntimeConfig,
        transport: aec_ai::SidecarTransport,
    ) -> Self {
        let mut runtime = SidecarRuntime::new(config);
        runtime.mark_ready();
        Self {
            runtime: RwLock::new(runtime),
            handle_slot: Mutex::new(Some(sidecar::adopt(transport))),
            pending_diffs: Mutex::new(HashMap::new()),
        }
    }

    /// Test-only: transition the runtime to `Loading` without going
    /// through `ensure_ready` (which would also do the blocking
    /// spawn). Used by the cold-spawn regression test to set up the
    /// "spawn in flight" world without a real child process.
    #[doc(hidden)]
    pub fn __test_begin_load(&self) {
        self.runtime.write().expect("runtime lock").begin_load();
    }

    /// Test-only: hold the `handle_slot` mutex for the given duration.
    /// Used by the cold-spawn regression test to simulate a spawn
    /// blocking on `/health` — any concurrent `snapshot()` call must
    /// still return instantly because it does NOT touch
    /// `handle_slot`. Run this on a background thread; the caller's
    /// thread then validates the latency of `snapshot()`.
    #[doc(hidden)]
    pub fn __test_hold_handle_slot_for(&self, dur: Duration) {
        let _guard = self.handle_slot.lock().expect("handle_slot lock");
        std::thread::sleep(dur);
    }

    /// Insert a diff into the pending map and return the assigned id.
    /// The `project_path` is captured alongside the diff so the
    /// later `ai_accept_diff` / `ai_reject_diff` calls (which only
    /// take a `diff_id`) can route to the correct project package.
    pub fn insert_diff(
        &self,
        project_path: impl Into<String>,
        scope: aec_core::types::Scope,
        diff: Diff,
    ) -> Result<DiffId, AiStateError> {
        let id = diff.id.clone();
        let pending = PendingDiff {
            project_path: project_path.into(),
            scope,
            diff,
        };
        self.pending_diffs
            .lock()
            .map_err(poisoned)?
            .insert(id.clone(), pending);
        Ok(id)
    }

    /// Remove and return the pending diff for `id`. The returned
    /// envelope carries both the `Diff` (for conversion to commands)
    /// and the `project_path` (for opening the package).
    pub fn accept_diff(&self, id: &str) -> Result<PendingDiff, AiStateError> {
        let key = DiffId::from_string(id.to_string())
            .map_err(|_| AiStateError::UnknownDiff(id.to_owned()))?;
        self.pending_diffs
            .lock()
            .map_err(poisoned)?
            .remove(&key)
            .ok_or_else(|| AiStateError::UnknownDiff(id.to_owned()))
    }

    /// Symmetric counterpart to [`Self::accept_diff`] — same return
    /// shape so the audit-logging path on the reject side can read
    /// both the diff (for `payload_hash`) and the project path (for
    /// the `<project>/audit/ai_audit.jsonl` destination).
    pub fn reject_diff(&self, id: &str) -> Result<PendingDiff, AiStateError> {
        let key = DiffId::from_string(id.to_string())
            .map_err(|_| AiStateError::UnknownDiff(id.to_owned()))?;
        self.pending_diffs
            .lock()
            .map_err(poisoned)?
            .remove(&key)
            .ok_or_else(|| AiStateError::UnknownDiff(id.to_owned()))
    }

    pub fn pending_diff_count(&self) -> Result<usize, AiStateError> {
        Ok(self.pending_diffs.lock().map_err(poisoned)?.len())
    }

    /// Force the sidecar to unload — called when the user explicitly
    /// cancels a long-running plan job, or when the bridge shuts down.
    /// Idempotent: cancelling when no sidecar is running is a no-op.
    ///
    /// Note: during a cold-spawn, `cancel_job` blocks on `handle_slot`
    /// until the spawn completes (success or failure). The cancel
    /// then drops the just-spawned handle and marks the runtime
    /// unloaded. From the renderer's perspective the UI stays
    /// responsive throughout — the napi `ai_cancel_job` runs on a
    /// blocking-pool worker thread (see [`crate::napi_api`]), so the
    /// libuv main thread is free during the wait.
    pub fn cancel_job(&self) -> Result<(), AiStateError> {
        let mut slot = self.handle_slot.lock().map_err(poisoned)?;
        if let Some(handle) = slot.take() {
            handle.shutdown();
            self.runtime.write().map_err(poisoned)?.mark_unloaded();
        }
        Ok(())
    }
}

impl Drop for AiState {
    /// On shutdown we kill the sidecar via `SidecarHandle::Drop`. Diffs
    /// are dropped naturally — they live in memory, not on disk.
    fn drop(&mut self) {
        if let Ok(mut slot) = self.handle_slot.lock() {
            if let Some(h) = slot.take() {
                // Explicitly invoke shutdown so we observe any kill
                // errors via panic-unsafe paths; `Drop` swallows them.
                h.shutdown();
            }
        }
    }
}
