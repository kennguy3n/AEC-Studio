//! Sidecar process management. Spawns `llama-server` (or any PrismML-compatible
//! binary) with the arguments the local sidecar config implies, polls its
//! `/health` endpoint until the model is loaded, and kills the child on Drop.
//!
//! The bridge holds a `SidecarHandle` inside `AiState` so the lifecycle is
//! bounded by the bridge instance — when the user closes the project, the
//! handle drops, kills the child, and the loopback port frees up before the
//! next session.
//!
//! ## Why this is a thin wrapper around `std::process`
//!
//! The Phase 10 PR-V scoping doc explicitly limits us to "spawn / health /
//! kill" — we do **not** speak the llama-server admin protocol, do not parse
//! its stderr, and do not stream stdout. Anything beyond "the child PID is
//! alive and the HTTP handshake succeeded" is left to the next phase.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::runtime::RuntimeConfig;
use crate::transport::SidecarTransport;

/// Env-var override for the `llama-server` binary path. Used by integration
/// tests (`AEC_AI_PRISMML_BIN=…`) and by the bridge when the snapshot bundles
/// a vendored binary. If unset, the spawner falls back to invoking the binary
/// by its PATH name.
pub const SIDECAR_BIN_ENV: &str = "AEC_AI_PRISMML_BIN";

/// Default binary name. Overridable via [`SIDECAR_BIN_ENV`].
pub const DEFAULT_SIDECAR_BIN: &str = "llama-server";

#[derive(Debug, Error)]
pub enum SidecarSpawnError {
    #[error("spawn `{bin}`: {source}")]
    Spawn {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("sidecar did not become ready within {0:?}")]
    HealthTimeout(Duration),
    #[error("sidecar exited before becoming ready: {0:?}")]
    EarlyExit(Option<i32>),
}

/// A live sidecar process. Killing the child on Drop is the only correctness-
/// critical invariant: the process holds the loopback port and a multi-GiB
/// mmap of the model file; leaking it would prevent the next bridge instance
/// from binding the same port and would pin the model in RAM until reboot.
#[derive(Debug)]
pub struct SidecarHandle {
    child: Option<Child>,
    transport: SidecarTransport,
}

impl SidecarHandle {
    pub fn transport(&self) -> &SidecarTransport {
        &self.transport
    }

    /// Try to read the child's exit status without blocking. Returns `Some`
    /// only if the child has already exited; otherwise `None`. The runtime
    /// uses this to flip from `Ready` to `Failed` if the sidecar crashes
    /// between requests.
    pub fn try_exit_code(&mut self) -> Option<i32> {
        let child = self.child.as_mut()?;
        child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.code().unwrap_or(-1))
    }

    /// Explicit kill — equivalent to `drop(handle)` but lets the caller
    /// observe any I/O error (Drop swallows them).
    pub fn shutdown(mut self) {
        self.kill_inner();
    }

    fn kill_inner(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort: ignore "already-exited" errors.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SidecarHandle {
    fn drop(&mut self) {
        self.kill_inner();
    }
}

/// Lookup the configured sidecar binary path, applying the
/// [`SIDECAR_BIN_ENV`] override.
pub fn sidecar_bin() -> PathBuf {
    if let Ok(p) = std::env::var(SIDECAR_BIN_ENV) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from(DEFAULT_SIDECAR_BIN)
}

/// Compile-time host-OS family. Captured into a value so the spawn arg
/// builder can be unit-tested for every target without recompiling: tests
/// pass a synthetic [`HostPlatform`] in instead of using `cfg!()`
/// directly. Production code reads `HostPlatform::current()`, which
/// resolves at compile time to whatever target the binary was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    /// macOS (any arch). Metal GPU offload via `--n-gpu-layers 999`.
    MacOs,
    /// Windows. CUDA / Vulkan offload + `--no-mmap` (Windows mmap of
    /// multi-GB GGUF files is unreliable on FAT-backed or network drives,
    /// and `llama-server` recommends disabling it on Windows).
    Windows,
    /// Linux (and other Unix). CUDA / Vulkan offload.
    Linux,
}

impl HostPlatform {
    /// Resolve the platform the current binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            // Treat every other Unix the same as Linux for spawn-arg
            // purposes — the GPU-offload + flash-attn + mmap behaviour
            // is identical on FreeBSD / OpenBSD / illumos.
            Self::Linux
        }
    }
}

/// Build the `llama-server` argv tail for `config` on `platform`. Returns
/// the full ordered arg list (including the always-on `--host` /
/// `--port` / `--model` flags) so the test suite can pin the exact
/// command-line shape per target.
///
/// Args added on top of the baseline (host / port / ctx-size / parallel /
/// model):
///
/// * `--n-gpu-layers 999` — request all-layer GPU offload. The PrismML
///   fork falls back to CPU for any layer that doesn't fit, so this is
///   safe to pass unconditionally; a CPU-only host simply ignores it
///   after the auto-detect. On macOS this routes through Metal; on
///   Windows / Linux through CUDA (NVIDIA), Vulkan (AMD / Intel), or HIP
///   (consumer AMD), whichever was compiled into the binary.
/// * `--flash-attn` — flash-attention kernels. Ternary-Bonsai Q2_0
///   supports it on every backend the PrismML fork ships, and it cuts
///   prefill latency \~30 % on Apple Silicon. There is no model where
///   we'd want to *disable* this once the kernel exists, so it is
///   unconditional.
/// * `--no-mmap` — Windows-only. Disables `MapViewOfFile`-based loading
///   for the model file; the fallback `read()` loop avoids the
///   intermittent EOF errors `llama.cpp` reports on Windows when the
///   GGUF lives on a network share or a non-NTFS drive. macOS / Linux
///   keep mmap on for the cold-load fault-in win.
///
/// The argv is built deterministically so `spawn_args_for_*` tests can
/// assert exact ordering and presence.
pub fn build_spawn_args(config: &RuntimeConfig, platform: HostPlatform) -> Vec<String> {
    let mut args = vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        config.port.to_string(),
        "--ctx-size".into(),
        config.max_context_tokens.to_string(),
        "--parallel".into(),
        config.parallel.to_string(),
        "--model".into(),
        config.model_path.display().to_string(),
        // GPU offload + flash-attn are platform-independent in their
        // *request* — the binary itself decides whether to honour them
        // based on the backend it was compiled with and the hardware
        // available at spawn time.
        "--n-gpu-layers".into(),
        "999".into(),
        "--flash-attn".into(),
    ];
    if platform == HostPlatform::Windows {
        args.push("--no-mmap".into());
    }
    args
}

/// Spawn the sidecar and wait until its `/health` endpoint reports `ok`.
///
/// `health_poll_timeout` is the overall budget — typically 30 s for cold-cache
/// model load; the spawner polls every 100 ms until that budget is exhausted.
///
/// The returned `SidecarHandle` *owns* the child process; dropping it kills
/// the sidecar. The transport inside the handle is pre-configured with the
/// runtime's request timeout.
///
/// Spawn args are built by [`build_spawn_args`] for [`HostPlatform::current`]
/// — see that function's doc for the per-target GPU-offload / mmap /
/// flash-attn matrix.
pub fn spawn(
    config: &RuntimeConfig,
    health_poll_timeout: Duration,
) -> Result<SidecarHandle, SidecarSpawnError> {
    let bin = sidecar_bin();
    let bin_display = bin.display().to_string();
    let mut cmd = Command::new(&bin);
    for arg in build_spawn_args(config, HostPlatform::current()) {
        cmd.arg(arg);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd.spawn().map_err(|source| SidecarSpawnError::Spawn {
        bin: bin_display,
        source,
    })?;
    let transport = SidecarTransport::new(config.port, config.request_timeout);
    let mut handle = SidecarHandle {
        child: Some(child),
        transport: transport.clone(),
    };
    poll_until_healthy(&transport, &mut handle, health_poll_timeout)?;
    Ok(handle)
}

/// Wrap a pre-existing sidecar process (one the bridge did not spawn).
/// Used by integration tests that spin up a mock TCP server and want the
/// runtime to treat it as if it were a real sidecar. Caller is responsible
/// for managing the foreign process's lifecycle.
pub fn adopt(transport: SidecarTransport) -> SidecarHandle {
    SidecarHandle {
        child: None,
        transport,
    }
}

fn poll_until_healthy(
    transport: &SidecarTransport,
    handle: &mut SidecarHandle,
    budget: Duration,
) -> Result<(), SidecarSpawnError> {
    let start = Instant::now();
    let tick = Duration::from_millis(100);
    loop {
        // If the child has already exited there's no point polling further.
        if let Some(code) = handle.try_exit_code() {
            return Err(SidecarSpawnError::EarlyExit(Some(code)));
        }
        if let Ok(true) = transport.health() {
            return Ok(());
        }
        if start.elapsed() >= budget {
            return Err(SidecarSpawnError::HealthTimeout(budget));
        }
        std::thread::sleep(tick);
    }
}

/// Restart policy with exponential backoff. Tracks the number of consecutive
/// failures and computes the next delay.
///
/// Delay schedule: each call to [`Self::record_failure`] first bumps the
/// consecutive-failure counter and then returns `500 ms * 2^counter`, capped
/// at the configured `max_delay` (default 30 s). So the first failure
/// returns 1 s (= 500 ms × 2), the second returns 2 s, the third 4 s, etc.
/// The base of 500 ms only shows up in the *pre*-increment state and is
/// never actually returned. Calling [`RestartPolicy::reset`] after a
/// successful health check resets the counter to zero, restarting the
/// schedule on the next failure.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    consecutive_failures: u32,
    max_delay: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RestartPolicy {
    pub fn new(max_delay: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            max_delay,
        }
    }

    /// Record a spawn failure. Returns the delay the caller should wait before
    /// the next attempt.
    pub fn record_failure(&mut self) -> Duration {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.next_delay()
    }

    /// Compute the delay for the current failure count without recording a
    /// new failure. Used by callers that want to inspect the policy before
    /// deciding whether to retry.
    pub fn next_delay(&self) -> Duration {
        let base_ms = 500_u64;
        let shift = self.consecutive_failures.min(20);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay_ms = base_ms.saturating_mul(multiplier);
        Duration::from_millis(delay_ms).min(self.max_delay)
    }

    /// Reset after a successful spawn + health check.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

/// Spawn the sidecar with automatic retry. On failure, the
/// [`RestartPolicy`] is advanced — `record_failure()` bumps the
/// consecutive-failure counter and returns the back-off duration the
/// caller should sleep before the next attempt. Up to `max_attempts`
/// total spawns are attempted.
///
/// Returns the live handle on the first successful attempt. If all
/// attempts fail, returns the error from the last attempt.
pub fn spawn_with_retry(
    config: &RuntimeConfig,
    health_poll_timeout: Duration,
    policy: &mut RestartPolicy,
    max_attempts: u32,
) -> Result<SidecarHandle, SidecarSpawnError> {
    let mut last_err = None;
    let mut next_delay: Option<Duration> = None;
    for _ in 0..max_attempts {
        if let Some(delay) = next_delay.take() {
            std::thread::sleep(delay);
        }
        match spawn(config, health_poll_timeout) {
            Ok(handle) => {
                policy.reset();
                return Ok(handle);
            }
            Err(e) => {
                // `record_failure` bumps the failure counter and returns
                // the exact delay to wait before the next attempt, so the
                // sleep above and the policy state are always in lock-step.
                next_delay = Some(policy.record_failure());
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(SidecarSpawnError::HealthTimeout(health_poll_timeout)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Process-global lock for env-var-mutating tests. Rust runs tests in a
    // single binary on multiple threads by default and `std::env::set_var`
    // is process-global, so the three tests below would race without this
    // (e.g. one test's `remove_var` clobbers another's `set_var`).
    // Using a `Mutex` over the much-recommended `serial_test` crate keeps
    // us dep-free — these three tests are the only env-mutating tests in
    // the crate.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Take the env lock, returning a guard that also restores the prior
    /// value of `SIDECAR_BIN_ENV` when it drops. Centralises the
    /// save/restore boilerplate that all env-mutating tests need.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn acquire() -> Self {
            // `lock().unwrap_or_else(..into_inner)` so a panicking sibling
            // test doesn't poison the lock and cascade-fail every other
            // env test in the same `cargo test` invocation.
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(SIDECAR_BIN_ENV).ok();
            std::env::remove_var(SIDECAR_BIN_ENV);
            Self { _lock: lock, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(p) => std::env::set_var(SIDECAR_BIN_ENV, p),
                None => std::env::remove_var(SIDECAR_BIN_ENV),
            }
        }
    }

    #[test]
    fn sidecar_bin_honours_env_override() {
        let _g = EnvGuard::acquire();
        std::env::set_var(SIDECAR_BIN_ENV, "/tmp/custom-llama");
        assert_eq!(sidecar_bin(), PathBuf::from("/tmp/custom-llama"));
        std::env::remove_var(SIDECAR_BIN_ENV);
        assert_eq!(sidecar_bin(), PathBuf::from(DEFAULT_SIDECAR_BIN));
    }

    #[test]
    fn empty_env_var_falls_back_to_default() {
        let _g = EnvGuard::acquire();
        std::env::set_var(SIDECAR_BIN_ENV, "");
        assert_eq!(sidecar_bin(), PathBuf::from(DEFAULT_SIDECAR_BIN));
    }

    #[test]
    fn spawn_with_missing_binary_reports_spawn_error() {
        let _g = EnvGuard::acquire();
        std::env::set_var(SIDECAR_BIN_ENV, "/nonexistent/aec-test-llama-server-xyz");
        let cfg = RuntimeConfig::default();
        let result = spawn(&cfg, Duration::from_millis(10));
        assert!(matches!(result, Err(SidecarSpawnError::Spawn { .. })));
    }

    #[test]
    fn restart_policy_doubles_delay_on_each_failure() {
        let mut policy = RestartPolicy::new(Duration::from_secs(30));
        assert_eq!(policy.consecutive_failures(), 0);
        let d1 = policy.record_failure();
        assert_eq!(d1, Duration::from_secs(1)); // 500 * 2^1
        let d2 = policy.record_failure();
        assert_eq!(d2, Duration::from_secs(2)); // 500 * 2^2
        let d3 = policy.record_failure();
        assert_eq!(d3, Duration::from_secs(4)); // 500 * 2^3
        assert_eq!(policy.consecutive_failures(), 3);
    }

    #[test]
    fn restart_policy_caps_at_max_delay() {
        let mut policy = RestartPolicy::new(Duration::from_secs(5));
        for _ in 0..20 {
            policy.record_failure();
        }
        assert!(policy.next_delay() <= Duration::from_secs(5));
    }

    #[test]
    fn restart_policy_resets_on_success() {
        let mut policy = RestartPolicy::default();
        policy.record_failure();
        policy.record_failure();
        assert_eq!(policy.consecutive_failures(), 2);
        policy.reset();
        assert_eq!(policy.consecutive_failures(), 0);
        assert_eq!(policy.next_delay(), Duration::from_millis(500));
    }

    /// The argv builder must include `--n-gpu-layers 999` and
    /// `--flash-attn` on every platform — PrismML auto-detects whether
    /// the backend can honour the offload request, so passing it on a
    /// CPU-only host is a no-op rather than a failure. Pinning this is
    /// what stops a future "let's drop GPU flags on Linux" regression
    /// from silently halving inference speed on every GPU machine.
    #[test]
    fn spawn_args_request_gpu_offload_and_flash_attn_on_every_platform() {
        let cfg = RuntimeConfig::default();
        for platform in [
            HostPlatform::MacOs,
            HostPlatform::Linux,
            HostPlatform::Windows,
        ] {
            let args = build_spawn_args(&cfg, platform);
            // `--n-gpu-layers 999` must appear as a pair.
            let ngl_idx = args
                .iter()
                .position(|a| a == "--n-gpu-layers")
                .unwrap_or_else(|| panic!("missing --n-gpu-layers on {platform:?}: {args:?}"));
            assert_eq!(
                args.get(ngl_idx + 1).map(String::as_str),
                Some("999"),
                "--n-gpu-layers must be followed by 999 on {platform:?}",
            );
            assert!(
                args.iter().any(|a| a == "--flash-attn"),
                "missing --flash-attn on {platform:?}: {args:?}",
            );
        }
    }

    /// `--no-mmap` is Windows-specific. The mmap fallback path inside
    /// `llama.cpp` reports intermittent EOF errors on Windows network
    /// drives and non-NTFS volumes; macOS / Linux mmap is reliable for
    /// multi-GB GGUF loads so we keep it on for the page-fault-in win.
    #[test]
    fn no_mmap_only_on_windows() {
        let cfg = RuntimeConfig::default();
        assert!(
            !build_spawn_args(&cfg, HostPlatform::MacOs).contains(&"--no-mmap".to_string()),
            "macOS must not pass --no-mmap (mmap is the fast path)",
        );
        assert!(
            !build_spawn_args(&cfg, HostPlatform::Linux).contains(&"--no-mmap".to_string()),
            "Linux must not pass --no-mmap (mmap is the fast path)",
        );
        assert!(
            build_spawn_args(&cfg, HostPlatform::Windows).contains(&"--no-mmap".to_string()),
            "Windows must pass --no-mmap (mmap is unreliable for large GGUF on Windows)",
        );
    }

    /// The baseline args (host, port, ctx-size, parallel, model) must
    /// appear before the platform-specific GPU / mmap / flash-attn tail.
    /// Their values must reflect the supplied [`RuntimeConfig`] so the
    /// `ai_set_active_tier` → `reload_with_config` chain (which mutates
    /// `model_path` and `max_context_tokens`) actually reaches the
    /// sidecar process on next spawn.
    #[test]
    fn spawn_args_reflect_runtime_config_values() {
        let cfg = RuntimeConfig {
            port: 17_823,
            max_context_tokens: 4096,
            parallel: 3,
            model_path: std::path::PathBuf::from("/fake/Ternary-Bonsai-8B-Q2_0.gguf"),
            ..RuntimeConfig::default()
        };
        let args = build_spawn_args(&cfg, HostPlatform::Linux);
        let pos = |needle: &str| {
            args.iter()
                .position(|a| a == needle)
                .unwrap_or_else(|| panic!("missing {needle}: {args:?}"))
        };
        assert_eq!(
            args.get(pos("--port") + 1).map(String::as_str),
            Some("17823")
        );
        assert_eq!(
            args.get(pos("--ctx-size") + 1).map(String::as_str),
            Some("4096"),
        );
        assert_eq!(
            args.get(pos("--parallel") + 1).map(String::as_str),
            Some("3"),
        );
        assert_eq!(
            args.get(pos("--model") + 1).map(String::as_str),
            Some("/fake/Ternary-Bonsai-8B-Q2_0.gguf"),
        );
    }

    #[test]
    fn spawn_with_retry_fails_after_max_attempts() {
        let _g = EnvGuard::acquire();
        std::env::set_var(SIDECAR_BIN_ENV, "/nonexistent/aec-test-xyz");
        let cfg = RuntimeConfig::default();
        let mut policy = RestartPolicy::new(Duration::from_millis(1)); // tiny for test speed
        let result = spawn_with_retry(&cfg, Duration::from_millis(1), &mut policy, 2);
        assert!(result.is_err());
        assert_eq!(policy.consecutive_failures(), 2);
    }
}
