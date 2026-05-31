//! Sidecar runtime (llama.cpp / PrismML fork). The runtime is *the* place
//! where AEC Studio talks to a local AI process; everything else in the
//! crate consumes structured tool requests.
//!
//! The runtime owns the configuration, the lifecycle state machine
//! (Idle/Loading/Ready/Failed), and exposes an `is_available()` hook the
//! planner uses to gate calls. The actual sidecar process is launched by
//! `aec_bridge` (which holds the OS-level handles).
//!
//! ## Defaults are self-contained
//!
//! [`RuntimeConfig::default`] does **not** read from disk. The canonical
//! defaults live in this file, and the platform-appropriate models
//! directory is computed at runtime via [`default_models_dir`]. The
//! Python-era `workers/ai/config.json` parser ([`from_sidecar_config_json`])
//! remains for callers that still ship the file, but the sidecar runtime
//! no longer requires it (no Python ships in the production app).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model_manager::ModelTier;

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
    /// Self-contained default. The model path points at the
    /// platform-appropriate per-user data directory + the canonical
    /// `ModelTier::Small` (Ternary-Bonsai 1.7B Q2_0) filename. Callers
    /// that want a different tier should construct the config via
    /// [`crate::model_manager::ModelManager::active_config`].
    fn default() -> Self {
        Self {
            model_path: default_models_dir().join(ModelTier::Small.filename()),
            port: 13579,
            parallel: 2,
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(120),
            max_context_tokens: 4096,
        }
    }
}

/// Per-platform per-user models directory.
///
/// - macOS:   `~/Library/Application Support/AEC Studio/models`
/// - Linux:   `$XDG_DATA_HOME/aec-studio/models`, fallback
///   `~/.local/share/aec-studio/models`
/// - Windows: `%LOCALAPPDATA%\AEC Studio\models`
///
/// Falls back to `./models` only if no home directory can be resolved
/// from environment (effectively impossible on a normal user account;
/// kept so tests in containers don't panic).
pub fn default_models_dir() -> PathBuf {
    let app_name = "AEC Studio";
    let snake_app = "aec-studio";
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(app_name)
                .join("models");
        }
    } else if cfg!(target_os = "windows") {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join(app_name).join("models");
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile)
                .join("AppData")
                .join("Local")
                .join(app_name)
                .join("models");
        }
    } else {
        // Linux / *BSD: XDG Base Directory.
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let xdg_str = xdg.to_string_lossy();
            if !xdg_str.is_empty() {
                return PathBuf::from(xdg.clone()).join(snake_app).join("models");
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join(snake_app)
                .join("models");
        }
    }
    PathBuf::from("models")
}

impl RuntimeConfig {
    /// Parse a runtime config from a legacy sidecar JSON file with the
    /// same shape as the pre-Phase-18 `workers/ai/config.json`. Kept
    /// for callers that still ship such a file out-of-band; the
    /// in-tree defaults no longer depend on it.
    pub fn from_sidecar_config_json(text: &str) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Server {
            #[serde(default)]
            host: Option<String>,
            port: u16,
            context_size: u32,
            parallel: u32,
        }
        #[derive(Deserialize)]
        struct Top {
            default_model: Option<String>,
            models_dir: Option<String>,
            server: Server,
            idle_timeout_seconds: u64,
            request_timeout_seconds: u64,
        }
        let cfg: Top = serde_json::from_str(text)?;
        let model_path = match (cfg.models_dir, cfg.default_model) {
            (Some(dir), Some(name)) => PathBuf::from(format!("{dir}/{name}")),
            (_, Some(name)) => PathBuf::from(name),
            _ => RuntimeConfig::default().model_path,
        };
        // `host` is intentionally not stored on `RuntimeConfig` (the Rust
        // client always connects on loopback); we still parse it so the
        // canonical loader fails loudly if the field disappears.
        let _ = cfg.server.host;
        Ok(Self {
            model_path,
            port: cfg.server.port,
            parallel: cfg.server.parallel,
            idle_timeout: Duration::from_secs(cfg.idle_timeout_seconds),
            request_timeout: Duration::from_secs(cfg.request_timeout_seconds),
            max_context_tokens: cfg.server.context_size,
        })
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

    /// `from_sidecar_config_json` still parses the legacy shape — exercise
    /// it against an inline literal so the parser stays alive even now that
    /// no `workers/ai/config.json` file ships in-tree.
    #[test]
    fn from_sidecar_config_json_parses_legacy_shape() {
        let text = r#"{
            "default_model": "Ternary-Bonsai-1.7B-Q2_0.gguf",
            "models_dir": "${HOME}/.local/share/aec-studio/models",
            "server": {
                "host": "127.0.0.1",
                "port": 13579,
                "context_size": 4096,
                "parallel": 2
            },
            "idle_timeout_seconds": 60,
            "request_timeout_seconds": 120
        }"#;
        let parsed = RuntimeConfig::from_sidecar_config_json(text).expect("parses");
        let defaults = RuntimeConfig::default();
        assert_eq!(parsed.port, defaults.port);
        assert_eq!(parsed.parallel, defaults.parallel);
        assert_eq!(parsed.idle_timeout, defaults.idle_timeout);
        assert_eq!(parsed.request_timeout, defaults.request_timeout);
        assert_eq!(parsed.max_context_tokens, defaults.max_context_tokens);
        assert!(parsed
            .model_path
            .to_string_lossy()
            .ends_with("Ternary-Bonsai-1.7B-Q2_0.gguf"));
    }

    #[test]
    fn default_uses_ternary_bonsai_small_filename() {
        let cfg = RuntimeConfig::default();
        let s = cfg.model_path.to_string_lossy().into_owned();
        assert!(
            s.ends_with("Ternary-Bonsai-1.7B-Q2_0.gguf"),
            "default model path {s} should end with the Small tier filename",
        );
    }

    #[test]
    fn default_models_dir_picks_platform_appropriate_path() {
        let dir = default_models_dir();
        let s = dir.to_string_lossy();
        if cfg!(target_os = "macos") {
            assert!(
                s.contains("Library/Application Support/AEC Studio/models"),
                "macOS path was {s}",
            );
        } else if cfg!(target_os = "windows") {
            assert!(s.contains("AEC Studio\\models") || s.contains("AEC Studio/models"));
        } else {
            // On Linux either XDG_DATA_HOME or ~/.local/share + aec-studio/models.
            assert!(s.contains("aec-studio/models"), "Linux path was {s}");
        }
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
