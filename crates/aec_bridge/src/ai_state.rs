//! Process-wide AI runtime state held by [`crate::service::BridgeService`].
//!
//! Owns three pieces of state behind a single [`Mutex`]:
//!
//! 1. A [`SidecarRuntime`] state machine (Idle / Loading / Ready / Failed).
//! 2. An optional [`SidecarHandle`] — the spawned `llama-server` child
//!    process + its loopback transport. `None` until the first
//!    `ai_plan` call lazily spawns it.
//! 3. A `HashMap<DiffId, Diff>` of pending diffs. The renderer accepts /
//!    rejects diffs by id; the bridge keeps the in-memory diff alive
//!    until accept/reject so the command engine has a chance to convert
//!    it into a real apply.
//!
//! The same single-`Mutex` rationale that applies to `RenderState` applies
//! here: every AI endpoint touches at least two of (runtime / handle /
//! pending diffs), and the renderer never benefits from "the diff map is
//! free while a completion is in flight" because a single AI panel
//! serialises its own user interactions client-side. See `service.rs`
//! lock-ordering doc for the broader pattern.

use std::collections::HashMap;
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
}

/// Snapshot of `[AiState`] suitable for returning to the renderer.
/// Carrying it as an owned struct (rather than a `&AiState`) frees the
/// caller to drop the mutex guard before serialising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiStatusSnapshot {
    pub state: RuntimeState,
    pub last_error: Option<String>,
    /// Currently-tracked pending diff ids — useful for the renderer's
    /// "AI sidebar" to enumerate proposals on reconnect.
    pub pending_diff_ids: Vec<String>,
}

/// Process-wide AI state. Held inside a `Mutex<AiState>` on the bridge.
pub struct AiState {
    runtime: SidecarRuntime,
    /// `None` until the first `ai_plan` call. Dropping it on shutdown
    /// kills the underlying `llama-server` child via
    /// `SidecarHandle::Drop`.
    handle: Option<SidecarHandle>,
    pending_diffs: HashMap<DiffId, Diff>,
}

impl AiState {
    /// Initialise with the sidecar config baked from
    /// `workers/ai/config.json`. Does NOT spawn the sidecar yet —
    /// spawning is deferred until the first `ai_plan` call so the
    /// renderer can boot without paying the model-load cost.
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            runtime: SidecarRuntime::new(config),
            handle: None,
            pending_diffs: HashMap::new(),
        }
    }

    pub fn state(&self) -> RuntimeState {
        self.runtime.state()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.runtime.last_error()
    }

    pub fn snapshot(&self) -> AiStatusSnapshot {
        AiStatusSnapshot {
            state: self.runtime.state(),
            last_error: self.runtime.last_error().map(str::to_owned),
            pending_diff_ids: self
                .pending_diffs
                .keys()
                .map(|id| id.as_str().to_owned())
                .collect(),
        }
    }

    /// Lazy spawn-if-needed and return a borrowed transport. The first
    /// call pays the cold-cache cost (`spawn_timeout`); subsequent calls
    /// see a `Ready` runtime and short-circuit.
    ///
    /// On spawn failure the runtime transitions to `Failed` and the
    /// error is propagated; the next call retries the spawn.
    pub fn ensure_ready(
        &mut self,
        spawn_timeout: Duration,
    ) -> Result<&aec_ai::SidecarTransport, AiStateError> {
        if matches!(self.runtime.state(), RuntimeState::Failed) {
            // Reset so a subsequent ensure_ready attempts to spawn again.
            self.runtime = SidecarRuntime::new(self.runtime.config().clone());
        }
        if self.handle.is_none() {
            self.runtime.begin_load();
            match sidecar::spawn(self.runtime.config(), spawn_timeout) {
                Ok(handle) => {
                    self.handle = Some(handle);
                    self.runtime.mark_ready();
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.runtime.mark_failed(msg.clone());
                    return Err(e.into());
                }
            }
        } else {
            self.runtime.record_use();
        }
        Ok(self.handle.as_ref().expect("handle set above").transport())
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
            runtime,
            handle: Some(sidecar::adopt(transport)),
            pending_diffs: HashMap::new(),
        }
    }

    /// Insert a diff into the pending map and return the assigned id.
    pub fn insert_diff(&mut self, diff: Diff) -> DiffId {
        let id = diff.id.clone();
        self.pending_diffs.insert(id.clone(), diff);
        id
    }

    pub fn accept_diff(&mut self, id: &str) -> Result<Diff, AiStateError> {
        let key = DiffId::from_string(id.to_string())
            .map_err(|_| AiStateError::UnknownDiff(id.to_owned()))?;
        self.pending_diffs
            .remove(&key)
            .ok_or_else(|| AiStateError::UnknownDiff(id.to_owned()))
    }

    pub fn reject_diff(&mut self, id: &str) -> Result<Diff, AiStateError> {
        let key = DiffId::from_string(id.to_string())
            .map_err(|_| AiStateError::UnknownDiff(id.to_owned()))?;
        self.pending_diffs
            .remove(&key)
            .ok_or_else(|| AiStateError::UnknownDiff(id.to_owned()))
    }

    pub fn pending_diff_count(&self) -> usize {
        self.pending_diffs.len()
    }

    /// Force the sidecar to unload — called when the user explicitly
    /// cancels a long-running plan job, or when the bridge shuts down.
    /// Idempotent: dropping the handle a second time is a no-op.
    pub fn cancel_job(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            self.runtime.mark_unloaded();
        }
    }
}

impl Drop for AiState {
    /// On shutdown we kill the sidecar via `SidecarHandle::Drop`. Diffs
    /// are dropped naturally — they live in memory, not on disk.
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // Explicitly invoke shutdown so we observe any kill errors
            // via panic-unsafe paths; `Drop` swallows them.
            h.shutdown();
        }
    }
}
