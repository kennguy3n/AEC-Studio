//! Sidecar runtime (llama.cpp / PrismML). The runtime is *the* place where
//! AEC Studio talks to a local AI process; everything else in the crate
//! consumes structured tool requests.
//!
//! In Phase 1 the runtime is in-process: it owns the configuration, the
//! lifecycle state machine (Idle/Loading/Ready/Failed), and exposes an
//! `is_available()` hook the planner uses to gate calls. The actual sidecar
//! process is launched by `aec_bridge` (which holds the OS-level handles).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("sidecar is unavailable: {0}")]
    Unavailable(String),
    #[error("sidecar timed out after {0:?}")]
    Timeout(Duration),
    #[error("sidecar crashed: {0}")]
    Crashed(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub model_path: PathBuf,
    pub port: u16,
    pub parallel: u32,
    /// How long the sidecar may sit idle before being unloaded.
    pub idle_timeout: Duration,
    /// Maximum time we'll wait for a tool-call response.
    pub request_timeout: Duration,
    pub max_context_tokens: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("./workers/ai/models/prismml-default.gguf"),
            port: 8723,
            parallel: 2,
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(120),
            max_context_tokens: 8192,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
pub struct SidecarRuntime {
    config: RuntimeConfig,
    state: RuntimeState,
    last_used: Option<Instant>,
    last_error: Option<String>,
}

impl SidecarRuntime {
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            state: RuntimeState::Idle,
            last_used: None,
            last_error: None,
        }
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn is_available(&self) -> bool {
        matches!(self.state, RuntimeState::Ready)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Move the runtime into [`RuntimeState::Loading`] in preparation for
    /// the sidecar process being spawned.
    pub fn begin_load(&mut self) {
        self.state = RuntimeState::Loading;
        self.last_error = None;
    }

    /// Signal that the sidecar finished its handshake.
    pub fn mark_ready(&mut self) {
        self.state = RuntimeState::Ready;
        self.last_used = Some(Instant::now());
    }

    pub fn mark_failed(&mut self, message: impl Into<String>) {
        self.state = RuntimeState::Failed;
        self.last_error = Some(message.into());
    }

    pub fn record_use(&mut self) {
        self.last_used = Some(Instant::now());
    }

    /// True if the runtime is ready *and* has been idle for longer than the
    /// configured timeout.
    pub fn should_unload(&self, now: Instant) -> bool {
        match (self.state, self.last_used) {
            (RuntimeState::Ready, Some(t)) => now.duration_since(t) > self.config.idle_timeout,
            _ => false,
        }
    }

    /// Move the runtime back to [`RuntimeState::Idle`] after the process
    /// was unloaded.
    pub fn mark_unloaded(&mut self) {
        self.state = RuntimeState::Idle;
        self.last_used = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn state_machine_transitions() {
        let mut r = SidecarRuntime::new(RuntimeConfig::default());
        assert_eq!(r.state(), RuntimeState::Idle);
        assert!(!r.is_available());
        r.begin_load();
        assert_eq!(r.state(), RuntimeState::Loading);
        r.mark_ready();
        assert!(r.is_available());
        r.mark_failed("oom");
        assert_eq!(r.state(), RuntimeState::Failed);
        assert_eq!(r.last_error(), Some("oom"));
    }

    #[test]
    fn idle_unload_triggers_after_timeout() {
        let cfg = RuntimeConfig {
            idle_timeout: Duration::from_millis(10),
            ..RuntimeConfig::default()
        };
        let mut r = SidecarRuntime::new(cfg);
        r.mark_ready();
        sleep(Duration::from_millis(20));
        assert!(r.should_unload(Instant::now()));
        r.mark_unloaded();
        assert_eq!(r.state(), RuntimeState::Idle);
        assert!(!r.should_unload(Instant::now()));
    }
}
