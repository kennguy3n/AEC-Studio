//! Image-gen runtime state machine.
//!
//! Twin of [`crate::runtime::SidecarRuntime`] for the image-gen
//! sidecar. We keep the two state machines structurally identical
//! (`Idle → Loading → Ready ↔ Failed → Idle`) so the bridge and the
//! renderer can reason about both lifecycles with the same vocabulary;
//! the differences are:
//!
//!   * Default idle-unload window is **120 s** (vs text's 60 s) —
//!     image-gen models are 5–10× the resident cost of text models so
//!     we evict more eagerly per second of idleness, but the absolute
//!     window is longer because the user's typical workflow toggles
//!     between text completions and image generations within a
//!     1–2 minute window.
//!   * The runtime carries an [`ImageGenRuntimeConfig`] with the
//!     active `ImageGenConfig` so the bridge can re-spawn after an
//!     idle unload without re-reading the model manager.
//!
//! Lifecycle invariants pinned by tests:
//!
//!   1. `Idle → Loading` only via [`ImageGenRuntime::begin_load`].
//!   2. `Loading → Ready` only via [`ImageGenRuntime::mark_ready`].
//!   3. `Ready → Idle` only when [`ImageGenRuntime::should_unload`]
//!      returns `true` *and* the bridge has shut down the handle.
//!   4. `Failed` is sticky until [`ImageGenRuntime::reset`] is called
//!      — the bridge surfaces the last error to the renderer and
//!      requires an explicit user retry to clear it (same shape the
//!      text runtime exposes).

use std::time::{Duration, Instant};

use thiserror::Error;

use super::sidecar::ImageGenConfig;

/// Default idle-unload window for the image-gen runtime.
pub const DEFAULT_IMAGE_GEN_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Default health-load budget for spawning the image-gen sidecar.
/// Generous because diffusion model load includes mmap of multi-GiB
/// weights *and* CUDA / Metal context init.
pub const DEFAULT_IMAGE_GEN_LOAD_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum ImageGenRuntimeError {
    #[error("image-gen runtime is not ready: {0:?}")]
    NotReady(ImageGenRuntimeState),
    #[error("image-gen runtime is in failed state: {0}")]
    Failed(String),
}

/// Lifecycle states the image-gen runtime can be in. Matches
/// [`crate::runtime::RuntimeState`] one-for-one so the bridge can
/// expose a single union shape across text and image runtimes
/// without per-side conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageGenRuntimeState {
    Idle,
    Loading,
    Ready,
    Failed,
}

/// Runtime config bundle. Cloneable so the bridge can hand a snapshot
/// to a spawning task without holding the runtime mutex across the
/// spawn.
#[derive(Debug, Clone)]
pub struct ImageGenRuntimeConfig {
    pub spawn_config: ImageGenConfig,
    /// How long the runtime may sit in `Ready` with no requests
    /// before [`ImageGenRuntime::should_unload`] returns `true`.
    pub idle_timeout: Duration,
    /// Health-poll budget passed to
    /// [`super::sidecar::spawn`] when transitioning out of
    /// `Loading`.
    pub load_budget: Duration,
}

impl Default for ImageGenRuntimeConfig {
    fn default() -> Self {
        Self {
            spawn_config: ImageGenConfig::default(),
            idle_timeout: DEFAULT_IMAGE_GEN_IDLE_TIMEOUT,
            load_budget: DEFAULT_IMAGE_GEN_LOAD_BUDGET,
        }
    }
}

/// State machine for the image-gen sidecar lifecycle.
#[derive(Debug, Clone)]
pub struct ImageGenRuntime {
    config: ImageGenRuntimeConfig,
    state: ImageGenRuntimeState,
    last_used: Option<Instant>,
    last_error: Option<String>,
}

impl ImageGenRuntime {
    pub fn new(config: ImageGenRuntimeConfig) -> Self {
        Self {
            config,
            state: ImageGenRuntimeState::Idle,
            last_used: None,
            last_error: None,
        }
    }

    pub fn state(&self) -> ImageGenRuntimeState {
        self.state
    }

    pub fn config(&self) -> &ImageGenRuntimeConfig {
        &self.config
    }

    pub fn is_available(&self) -> bool {
        matches!(self.state, ImageGenRuntimeState::Ready)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// `Idle → Loading`. Idempotent re-entry from `Loading` is
    /// allowed (e.g. a poll thread observing a race), but
    /// `Ready → Loading` and `Failed → Loading` are rejected — the
    /// bridge must call [`Self::reset`] first.
    pub fn begin_load(&mut self) -> Result<(), ImageGenRuntimeError> {
        match self.state {
            ImageGenRuntimeState::Idle | ImageGenRuntimeState::Loading => {
                self.state = ImageGenRuntimeState::Loading;
                self.last_error = None;
                Ok(())
            }
            ImageGenRuntimeState::Ready => Err(ImageGenRuntimeError::NotReady(self.state)),
            ImageGenRuntimeState::Failed => Err(ImageGenRuntimeError::Failed(
                self.last_error.clone().unwrap_or_default(),
            )),
        }
    }

    /// `Loading → Ready`. No-op if already `Ready` (idempotent for
    /// concurrent spawn races).
    pub fn mark_ready(&mut self) {
        self.state = ImageGenRuntimeState::Ready;
        self.last_used = Some(Instant::now());
    }

    /// `* → Failed`. Sticky until [`Self::reset`].
    pub fn mark_failed(&mut self, message: impl Into<String>) {
        self.state = ImageGenRuntimeState::Failed;
        self.last_error = Some(message.into());
    }

    /// Stamp "request just completed" — bumps the idle clock.
    pub fn record_use(&mut self) {
        self.last_used = Some(Instant::now());
    }

    /// `* → Idle`. Used by both:
    ///   * the idle-unload path (`Ready → Idle` after the bridge
    ///     shuts down the handle);
    ///   * the explicit `reset()` path (`Failed → Idle` after the
    ///     user retries).
    pub fn reset(&mut self) {
        self.state = ImageGenRuntimeState::Idle;
        self.last_used = None;
        self.last_error = None;
    }

    /// `true` iff the runtime is `Ready` and the idle window has
    /// elapsed. The governor calls this on a 5 s tick to drive
    /// eager eviction.
    pub fn should_unload(&self, now: Instant) -> bool {
        match (self.state, self.last_used) {
            (ImageGenRuntimeState::Ready, Some(t)) => {
                now.duration_since(t) > self.config.idle_timeout
            }
            _ => false,
        }
    }

    /// Replace the spawn config (e.g. after the renderer's model
    /// picker selects a different image-gen variant). Forces the
    /// runtime back to `Idle` because the cached `Ready` state
    /// points at the *old* config.
    pub fn reload_with_config(&mut self, config: ImageGenRuntimeConfig) {
        self.config = config;
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_pinned() {
        let cfg = ImageGenRuntimeConfig::default();
        assert_eq!(cfg.idle_timeout, Duration::from_secs(120));
        assert_eq!(cfg.load_budget, Duration::from_secs(60));
    }

    #[test]
    fn lifecycle_idle_loading_ready_failed_reset() {
        let mut r = ImageGenRuntime::new(ImageGenRuntimeConfig::default());
        assert_eq!(r.state(), ImageGenRuntimeState::Idle);
        assert!(!r.is_available());
        r.begin_load().unwrap();
        assert_eq!(r.state(), ImageGenRuntimeState::Loading);
        r.mark_ready();
        assert_eq!(r.state(), ImageGenRuntimeState::Ready);
        assert!(r.is_available());
        r.mark_failed("oom");
        assert_eq!(r.state(), ImageGenRuntimeState::Failed);
        assert_eq!(r.last_error(), Some("oom"));
        // Failed is sticky — begin_load refuses until reset.
        let err = r.begin_load().unwrap_err();
        assert!(matches!(err, ImageGenRuntimeError::Failed(_)));
        r.reset();
        assert_eq!(r.state(), ImageGenRuntimeState::Idle);
        assert!(r.last_error().is_none());
        r.begin_load().unwrap();
        assert_eq!(r.state(), ImageGenRuntimeState::Loading);
    }

    #[test]
    fn ready_to_loading_requires_explicit_reset() {
        let mut r = ImageGenRuntime::new(ImageGenRuntimeConfig::default());
        r.begin_load().unwrap();
        r.mark_ready();
        let err = r.begin_load().unwrap_err();
        assert!(matches!(err, ImageGenRuntimeError::NotReady(_)));
        r.reset();
        r.begin_load().unwrap();
    }

    #[test]
    fn should_unload_requires_ready_and_idle_window_elapsed() {
        let cfg = ImageGenRuntimeConfig {
            idle_timeout: Duration::from_millis(20),
            ..Default::default()
        };
        let mut r = ImageGenRuntime::new(cfg);
        // Not Ready — should_unload is always false.
        assert!(!r.should_unload(Instant::now()));
        r.begin_load().unwrap();
        r.mark_ready();
        // Just used — should_unload still false.
        assert!(!r.should_unload(Instant::now()));
        std::thread::sleep(Duration::from_millis(30));
        assert!(r.should_unload(Instant::now()));
    }

    #[test]
    fn reload_with_config_replaces_spawn_config_and_resets_to_idle() {
        let mut r = ImageGenRuntime::new(ImageGenRuntimeConfig::default());
        r.begin_load().unwrap();
        r.mark_ready();
        assert_eq!(r.state(), ImageGenRuntimeState::Ready);
        let new_cfg = ImageGenRuntimeConfig {
            spawn_config: super::ImageGenConfig {
                port: 13599,
                ..super::ImageGenConfig::default()
            },
            ..Default::default()
        };
        r.reload_with_config(new_cfg);
        assert_eq!(r.state(), ImageGenRuntimeState::Idle);
        assert_eq!(r.config().spawn_config.port, 13599);
    }

    #[test]
    fn idempotent_begin_load_from_loading() {
        let mut r = ImageGenRuntime::new(ImageGenRuntimeConfig::default());
        r.begin_load().unwrap();
        // Re-entry from Loading is allowed (concurrent poll thread).
        r.begin_load().unwrap();
        assert_eq!(r.state(), ImageGenRuntimeState::Loading);
    }

    #[test]
    fn record_use_bumps_idle_clock() {
        let cfg = ImageGenRuntimeConfig {
            idle_timeout: Duration::from_millis(20),
            ..Default::default()
        };
        let mut r = ImageGenRuntime::new(cfg);
        r.begin_load().unwrap();
        r.mark_ready();
        std::thread::sleep(Duration::from_millis(15));
        // Bump.
        r.record_use();
        // Even after the original idle window elapses, we should
        // still be far from the new deadline.
        std::thread::sleep(Duration::from_millis(10));
        assert!(!r.should_unload(Instant::now()));
    }
}
