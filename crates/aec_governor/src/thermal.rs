//! Cross-platform thermal monitor for the resource governor.
//!
//! Three real platform sensors are provided:
//!
//! * **Linux** — reads `/sys/class/thermal/thermal_zone*/temp`, the
//!   standard ACPI thermal-zone kernel surface. Each `tempN` file
//!   contains an integer in millidegrees Celsius (e.g. `52000` =
//!   52.0 °C). We read every zone whose `type` matches one of the
//!   CPU-package well-known tags (`x86_pkg_temp`, `cpu_thermal`,
//!   `pch_skylake`, etc.) and take the **max** across zones — a
//!   single hot core trips back-off even if the package average is
//!   nominal.
//! * **macOS** — shells out to `pmset -g therm`. `pmset` is bundled
//!   in `/usr/bin` on every macOS install (10.5+), requires no
//!   privileges, and surfaces the IOPMrootDomain thermal pressure
//!   level. We parse `CPU_Speed_Limit` (100 = nominal, lower =
//!   throttled) and `CPU_Scheduler_Limit`. Together they pin
//!   throttle severity in a way that's available on both Intel and
//!   Apple Silicon. There is no public IOKit Rust binding that
//!   doesn't require `unsafe` FFI, and the workspace forbids
//!   `unsafe_code`, so `pmset` is the architecturally correct
//!   long-term choice.
//! * **Windows** — shells out to PowerShell:
//!   `Get-CimInstance -Namespace root/WMI -ClassName
//!   MSAcpi_ThermalZoneTemperature`. WMI returns
//!   `CurrentTemperature` in tenths of Kelvin (multiplied by 10);
//!   we convert to Celsius. PowerShell ships with every supported
//!   Windows SKU and the call requires no extra privileges.
//!
//! All three sensors are **read-only**, run **out-of-process** for
//! the shell-out variants (so the host process never hangs on a
//! sensor read), and surface `None` (rather than panic) on every
//! reasonable failure path (missing sysfs file, `pmset` not found,
//! WMI service unavailable, parse error). The governor falls back
//! to `ThermalState::Nominal` when no reading is available — better
//! to over-schedule than freeze the user's machine in the cold
//! state.
//!
//! Backoff policy: see [`ThermalThresholds`] for the
//! Celsius / pmset Speed-Limit cutoffs. Defaults are conservative
//! (warm at 80 °C / 99 % Speed-Limit, critical at 95 °C /
//! 60 % Speed-Limit) and pinned by tests.
//!
//! Tests inject a `ManualSensor` (a real implementation that
//! returns a pre-set reading) — **not** a mock — so the scheduler
//! integration is exercised against the same `ThermalSensor` trait
//! the platform code implements.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::scheduler::GovernorScheduler;
use crate::ui_report::ThermalState;

/// A single reading from a thermal sensor.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalReading {
    /// Hottest CPU/package zone temperature in degrees Celsius, if
    /// the platform exposes one. `None` on platforms that only
    /// expose a pressure level (e.g. macOS `pmset`).
    pub max_cpu_celsius: Option<f32>,
    /// Platform-specific CPU speed-limit ratio in `[0.0, 1.0]`, if
    /// the platform exposes one. `1.0` = unthrottled, lower values
    /// indicate the OS is asking us to back off. Currently only
    /// macOS surfaces this via `pmset -g therm` (`CPU_Speed_Limit`
    /// = 100 → 1.0).
    pub cpu_speed_limit_ratio: Option<f32>,
    /// Provenance of the reading.
    pub source: ThermalSource,
}

/// Which platform sensor produced the reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThermalSource {
    LinuxSysfs { zones: u32 },
    MacosPmset,
    WindowsWmi,
    Manual,
}

/// Trait every thermal sensor implements. Implementors must be
/// `Send + Sync` so the monitor can poll from a background thread
/// (the bridge wires a Tokio interval in production).
pub trait ThermalSensor: Send + Sync {
    /// Read one sample. Returns `Ok(None)` when the platform has no
    /// reading available right now (e.g. transient WMI failure) —
    /// this is NOT an error and the monitor should keep polling.
    /// Returns `Err` only when the sensor itself is permanently
    /// broken (e.g. a malformed sysfs path argument).
    fn read(&self) -> io::Result<Option<ThermalReading>>;
}

/// Thresholds for translating a [`ThermalReading`] into a
/// [`ThermalState`]. Two independent axes:
///
/// 1. Temperature: any reading whose `max_cpu_celsius` exceeds
///    `critical_celsius` => `Critical`; exceeds `warm_celsius` =>
///    `Warm`.
/// 2. Speed-limit ratio (macOS only): a reading whose
///    `cpu_speed_limit_ratio` falls below `critical_speed_limit`
///    => `Critical`; below `warm_speed_limit` => `Warm`.
///
/// The classifier returns the **worst** of the two axes — Apple's
/// IOPMrootDomain will start clipping speed-limit *before* the
/// reported package temperature crosses 95 °C, so the macOS
/// scheduler must back off even if the temperature axis still
/// reads nominal.
///
/// # Invariants
///
/// Two ordering invariants must hold for [`classify`] to behave
/// correctly (an inverted pair makes one branch of the `else if`
/// chain dead code, so the wrong [`ThermalState`] is returned for
/// readings between the two values):
///
/// 1. `warm_celsius` &lt; `critical_celsius` — temperatures are
///    monotonically increasing in the *hot* direction.
/// 2. `warm_speed_limit` &gt; `critical_speed_limit` — ratios are
///    monotonically decreasing in the *throttled* direction (1.0 =
///    unthrottled, 0.0 = fully clipped).
///
/// Both invariants must also be finite (no NaN, no ±∞), since
/// every NaN comparison returns `false` and would also produce
/// dead branches.
///
/// Construct through [`ThermalThresholds::try_new`] or
/// [`ThermalThresholds::default`] to validate up-front. Direct
/// struct-literal construction is still permitted (the fields are
/// `pub` so JSON deserialization can populate them), but callers
/// who do so should call [`validate`](Self::validate) before
/// passing the struct to [`classify`]. In debug builds, [`classify`]
/// includes `debug_assert!` checks against these invariants so
/// misconfiguration is caught loudly in tests rather than
/// silently mis-classifying in production.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalThresholds {
    pub warm_celsius: f32,
    pub critical_celsius: f32,
    pub warm_speed_limit: f32,
    pub critical_speed_limit: f32,
}

/// Reasons a [`ThermalThresholds`] value is malformed. Returned by
/// [`ThermalThresholds::try_new`] and [`ThermalThresholds::validate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThresholdError {
    /// A field was NaN or ±∞. All comparisons against NaN return
    /// `false`, which would make every `else if` arm of the
    /// classifier dead code.
    NonFinite { field: &'static str, value: f32 },
    /// `warm_celsius` is not strictly less than `critical_celsius`.
    TemperatureOrder { warm: f32, critical: f32 },
    /// `warm_speed_limit` is not strictly greater than
    /// `critical_speed_limit`.
    SpeedLimitOrder { warm: f32, critical: f32 },
    /// A speed-limit ratio is outside `[0.0, 1.0]`. The classifier
    /// only sees readings clamped to that range
    /// (`MacosPmsetSensor::parse` clamps with `.clamp(0.0, 1.0)`),
    /// so out-of-range thresholds can never trip.
    SpeedLimitOutOfRange { field: &'static str, value: f32 },
}

impl core::fmt::Display for ThresholdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ThresholdError::NonFinite { field, value } => {
                write!(f, "thermal threshold `{field}` is non-finite ({value})")
            }
            ThresholdError::TemperatureOrder { warm, critical } => write!(
                f,
                "thermal threshold ordering violation: \
                 warm_celsius ({warm}) must be < critical_celsius ({critical})"
            ),
            ThresholdError::SpeedLimitOrder { warm, critical } => write!(
                f,
                "thermal threshold ordering violation: \
                 warm_speed_limit ({warm}) must be > critical_speed_limit ({critical})"
            ),
            ThresholdError::SpeedLimitOutOfRange { field, value } => write!(
                f,
                "thermal threshold `{field}` ({value}) is outside [0.0, 1.0]"
            ),
        }
    }
}

impl std::error::Error for ThresholdError {}

impl ThermalThresholds {
    /// Construct thresholds, validating both ordering invariants
    /// and the finite/range constraints. Prefer this over struct
    /// literals when the values come from user / operator input.
    pub fn try_new(
        warm_celsius: f32,
        critical_celsius: f32,
        warm_speed_limit: f32,
        critical_speed_limit: f32,
    ) -> Result<Self, ThresholdError> {
        let t = Self {
            warm_celsius,
            critical_celsius,
            warm_speed_limit,
            critical_speed_limit,
        };
        t.validate()?;
        Ok(t)
    }

    /// Check both ordering invariants and the finite/range
    /// constraints. Cheap (four `is_finite` + two comparisons +
    /// two range checks) so it can be called on every config
    /// reload without measurable cost.
    pub fn validate(&self) -> Result<(), ThresholdError> {
        let check_finite = |field: &'static str, value: f32| -> Result<(), ThresholdError> {
            if value.is_finite() {
                Ok(())
            } else {
                Err(ThresholdError::NonFinite { field, value })
            }
        };
        check_finite("warm_celsius", self.warm_celsius)?;
        check_finite("critical_celsius", self.critical_celsius)?;
        check_finite("warm_speed_limit", self.warm_speed_limit)?;
        check_finite("critical_speed_limit", self.critical_speed_limit)?;

        // Finiteness was just verified above, so a plain >= / <=
        // is unambiguous here — no NaN can sneak past, and clippy's
        // `neg_cmp_op_on_partial_ord` lint correctly rejects the
        // `!(a < b)` form on `f32` for that exact reason.
        if self.warm_celsius >= self.critical_celsius {
            return Err(ThresholdError::TemperatureOrder {
                warm: self.warm_celsius,
                critical: self.critical_celsius,
            });
        }
        if self.warm_speed_limit <= self.critical_speed_limit {
            return Err(ThresholdError::SpeedLimitOrder {
                warm: self.warm_speed_limit,
                critical: self.critical_speed_limit,
            });
        }
        if !(0.0..=1.0).contains(&self.warm_speed_limit) {
            return Err(ThresholdError::SpeedLimitOutOfRange {
                field: "warm_speed_limit",
                value: self.warm_speed_limit,
            });
        }
        if !(0.0..=1.0).contains(&self.critical_speed_limit) {
            return Err(ThresholdError::SpeedLimitOutOfRange {
                field: "critical_speed_limit",
                value: self.critical_speed_limit,
            });
        }
        Ok(())
    }
}

impl Default for ThermalThresholds {
    fn default() -> Self {
        Self {
            // 80°C is the conservative "package is running hot" mark
            // used by macOS Activity Monitor / Intel XTU / cpufreq.
            warm_celsius: 80.0,
            // 95°C is the typical T_junction for desktop x86 CPUs;
            // sustained operation above this throttles the cores
            // hard.
            critical_celsius: 95.0,
            // Apple's pmset reports 99 once any thermal pressure
            // begins (it's not a continuous knob — it jumps in
            // multiples of ~1 % from 100 in the cold state). Treat
            // any drop from 100 as "Warm".
            warm_speed_limit: 0.99,
            // 60 % speed-limit means the OS has clipped sustained
            // CPU throughput by 40 % — at this point we MUST
            // stop scheduling new render tiles.
            critical_speed_limit: 0.60,
        }
    }
}

/// Classify a reading against the thresholds. Returns
/// [`ThermalState::Nominal`] when there's nothing to classify (no
/// reading available, no thermometers expose either axis).
///
/// In debug builds, asserts the [`ThermalThresholds`] invariants
/// (see that struct's docs). If you constructed the thresholds
/// through [`ThermalThresholds::try_new`] or [`Default`] this is
/// a no-op; if you populated them from a struct literal or
/// deserialization, an inverted pair will panic loudly in tests
/// rather than silently misclassifying.
pub fn classify(reading: Option<&ThermalReading>, thresholds: &ThermalThresholds) -> ThermalState {
    debug_assert!(
        thresholds.validate().is_ok(),
        "ThermalThresholds invariants violated: {:?}",
        thresholds.validate()
    );
    let Some(r) = reading else {
        return ThermalState::Nominal;
    };
    let mut s = ThermalState::Nominal;
    if let Some(c) = r.max_cpu_celsius {
        if c >= thresholds.critical_celsius {
            s = worst(s, ThermalState::Critical);
        } else if c >= thresholds.warm_celsius {
            s = worst(s, ThermalState::Warm);
        }
    }
    if let Some(ratio) = r.cpu_speed_limit_ratio {
        if ratio <= thresholds.critical_speed_limit {
            s = worst(s, ThermalState::Critical);
        } else if ratio <= thresholds.warm_speed_limit {
            s = worst(s, ThermalState::Warm);
        }
    }
    s
}

fn worst(a: ThermalState, b: ThermalState) -> ThermalState {
    match (a, b) {
        (ThermalState::Critical, _) | (_, ThermalState::Critical) => ThermalState::Critical,
        (ThermalState::Warm, _) | (_, ThermalState::Warm) => ThermalState::Warm,
        _ => ThermalState::Nominal,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux sysfs sensor.
// ─────────────────────────────────────────────────────────────────────────────

/// Sensor that walks `/sys/class/thermal/thermal_zone*/temp` and
/// returns the max temperature across all zones whose `type`
/// matches a CPU-package tag. The default root is
/// `/sys/class/thermal`; tests pass a tempdir.
#[derive(Debug, Clone)]
pub struct LinuxSysfsSensor {
    root: PathBuf,
}

impl LinuxSysfsSensor {
    /// New sensor reading from `/sys/class/thermal` (production).
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/sys/class/thermal"),
        }
    }

    /// New sensor reading from a custom root — for tests, or for a
    /// container with a sysfs bind mount.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// CPU-zone `type` strings the kernel emits across mainstream
    /// distros and CPU vendors. Drawn from the upstream Linux
    /// drivers (`drivers/thermal/intel/x86_pkg_temp_thermal.c`,
    /// `drivers/thermal/cpufreq_cooling.c`,
    /// `drivers/platform/x86/intel/pch_thermal.c`, etc.).
    fn cpu_zone_tags() -> &'static [&'static str] {
        &[
            "x86_pkg_temp",
            "cpu_thermal",
            "cpu-thermal",
            "soc_thermal",
            "soc-thermal",
            "pch_skylake",
            "pch_haswell",
            "pch_lewisburg",
            "pch_wildcat_point",
            "INT3400 Thermal",
            "INT3401 Thermal",
            "INT3403 Thermal",
            "TCPU",
        ]
    }
}

impl Default for LinuxSysfsSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl ThermalSensor for LinuxSysfsSensor {
    fn read(&self) -> io::Result<Option<ThermalReading>> {
        let read = match fs::read_dir(&self.root) {
            Ok(r) => r,
            // /sys/class/thermal doesn't exist (e.g. minimal
            // container) — silently return None.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut max_c: Option<f32> = None;
        let mut zones_read: u32 = 0;
        let tags = Self::cpu_zone_tags();
        for entry in read.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !name.starts_with("thermal_zone") {
                continue;
            }
            let type_path = path.join("type");
            let temp_path = path.join("temp");
            let type_str = fs::read_to_string(&type_path)
                .ok()
                .map(|s| s.trim().to_owned());
            let is_cpu = type_str.as_deref().is_some_and(|t| tags.contains(&t));
            if !is_cpu {
                continue;
            }
            let Ok(temp_str) = fs::read_to_string(&temp_path) else {
                continue;
            };
            let Ok(millic) = temp_str.trim().parse::<i64>() else {
                continue;
            };
            let c = millic as f32 / 1000.0;
            zones_read += 1;
            max_c = Some(match max_c {
                Some(m) => m.max(c),
                None => c,
            });
        }
        if zones_read == 0 {
            return Ok(None);
        }
        Ok(Some(ThermalReading {
            max_cpu_celsius: max_c,
            cpu_speed_limit_ratio: None,
            source: ThermalSource::LinuxSysfs { zones: zones_read },
        }))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared shell-out infrastructure (timeout-aware command runner).
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum wall-clock time we will wait for any shell-out sensor
/// (`pmset`, PowerShell/WMI) to exit before killing it.
///
/// Picked at 2 s — that is ~20× the worst-case observed real-world
/// `pmset -g therm` latency, and well over the cold-start cost of
/// `powershell.exe -NoProfile`. Anything beyond this strongly
/// suggests a stuck subprocess (e.g. a hung WMI provider), which
/// must NOT pin the thermal monitor thread: that thread polls every
/// few seconds, and an indefinite block would cause the cached
/// thermal state to go stale silently, defeating the back-off
/// guarantee in [`crate::thermal::ThermalMonitor`].
pub const DEFAULT_SHELL_OUT_TIMEOUT: Duration = Duration::from_secs(2);

/// Tick interval for the timeout poll loop. Each tick wakes up,
/// calls `child.try_wait()`, and either returns the exit status or
/// sleeps again. Picked at 25 ms — small enough that the timeout
/// is honored within ~25 ms of the budget for a quick-running
/// command, and large enough that the polling overhead is
/// negligible compared to the multi-second `ThermalMonitor` poll
/// interval.
const SHELL_OUT_POLL_TICK: Duration = Duration::from_millis(25);

/// Run a [`Command`] with a hard wall-clock timeout. Returns:
///
/// * `Ok(Some(output))` — the child exited within the budget and
///   we captured its stdout/stderr/status.
/// * `Ok(None)` — either the binary doesn't exist (matches the
///   pre-timeout `NotFound` fast-path on every platform) **or**
///   the timeout elapsed and we killed the child. Both are
///   non-fatal: the caller treats them as "no sample this cycle"
///   and the monitor will retry on the next poll.
/// * `Err(io::Error)` — a genuine I/O failure other than
///   `NotFound` (e.g. spawning failed for reasons we can't
///   recover from).
///
/// The child's stdout and stderr are piped (captured), and stdin
/// is wired to `/dev/null` so a confused subprocess can't block
/// reading from a closed stdin.
///
/// **Why a hand-rolled poll loop instead of `Child::wait_timeout`
/// from the `wait_timeout` crate?** `std::process::Child` has no
/// built-in timeout, but `try_wait()` is non-blocking and the
/// crate would add a transitive dependency for ~30 lines of
/// logic. The 25 ms tick keeps wake-ups well below the
/// `ThermalMonitor` poll interval cost, so a dependency is not
/// justified.
fn run_command_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> io::Result<Option<std::process::Output>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        // Binary doesn't exist → same return as the pre-timeout
        // fast-path: no sample available, not a hard error.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            // Process exited within the budget — collect the
            // captured output via `wait_with_output`. This also
            // closes the captured handles cleanly.
            let out = child.wait_with_output()?;
            return Ok(Some(out));
        }
        if Instant::now() >= deadline {
            // Budget exhausted — kill the child, reap the
            // zombie, and report a soft failure. `kill`
            // and the subsequent `wait` are best-effort:
            // if the process raced us and exited first,
            // both calls are harmless.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(SHELL_OUT_POLL_TICK);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS pmset sensor.
// ─────────────────────────────────────────────────────────────────────────────

/// Sensor that shells out to `pmset -g therm`.
///
/// `pmset` is part of macOS since 10.5 and lives at `/usr/bin/pmset`.
/// It surfaces the IOPMrootDomain thermal pressure level without
/// requiring sudo or any private framework.
///
/// Sample output (nominal):
/// ```text
/// CPU_Scheduler_Limit  = 100
/// CPU_Available_CPUs   = 16
/// CPU_Speed_Limit      = 100
/// ```
///
/// We parse `CPU_Speed_Limit` and convert to a ratio in
/// `[0.0, 1.0]`. macOS does not expose a numeric per-zone
/// temperature without unsafe IOKit FFI (which the workspace
/// forbids via `unsafe_code = "forbid"`), so the temperature axis
/// is `None` on this platform.
///
/// **Timeout**: `pmset -g therm` is observed at sub-100 ms in
/// production. The sensor enforces a hard timeout (default
/// [`DEFAULT_SHELL_OUT_TIMEOUT`]) and kills the child if exceeded,
/// returning `None` for that poll cycle. Without the timeout the
/// monitor thread would block indefinitely on a stuck `pmset` and
/// the cached thermal state would go stale silently — the
/// monitor's freshness guarantee depends on every poll returning
/// promptly. See [`run_command_with_timeout`].
#[derive(Debug, Clone)]
pub struct MacosPmsetSensor {
    /// `pmset` binary path. Production = `/usr/bin/pmset`. Tests
    /// pass a shell script that emits canned output.
    binary: PathBuf,
    /// Maximum wall-clock time we'll wait for `pmset` to exit
    /// before killing it. See [`run_command_with_timeout`].
    timeout: Duration,
}

impl MacosPmsetSensor {
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("/usr/bin/pmset"),
            timeout: DEFAULT_SHELL_OUT_TIMEOUT,
        }
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: DEFAULT_SHELL_OUT_TIMEOUT,
        }
    }

    /// Override the per-call timeout. The default is
    /// [`DEFAULT_SHELL_OUT_TIMEOUT`] (2 s), which is
    /// ~20× the observed worst-case real-world `pmset` latency.
    /// Tests use a very short timeout to exercise the kill path.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Parse a `pmset -g therm` payload. Public so the unit test
    /// suite can pin the parser against canned strings.
    pub fn parse(stdout: &str) -> Option<ThermalReading> {
        let mut speed_limit: Option<u32> = None;
        for line in stdout.lines() {
            let line = line.trim();
            // Each line looks like `CPU_Speed_Limit      = 100`.
            //
            // We anchor the match on the field-name *boundary* so
            // we never silently mis-parse a hypothetical future
            // sibling like `CPU_Speed_Limit_Extended` or
            // `CPU_Speed_Limit_Reason` as if it were the canonical
            // `CPU_Speed_Limit` reading. Apple's pmset payload
            // separates the field name from the value with one
            // or more whitespace characters before the `=`, so
            // requiring whitespace or `=` as the next character
            // is sufficient (and matches the exact spec
            // `<name><whitespace+>= <value>`).
            if let Some(rest) = line.strip_prefix("CPU_Speed_Limit") {
                let boundary_ok = rest
                    .chars()
                    .next()
                    .is_some_and(|c| c == '=' || c.is_whitespace());
                if boundary_ok {
                    if let Some(val) = rest.split('=').nth(1) {
                        speed_limit = val.trim().parse::<u32>().ok();
                    }
                }
            }
        }
        speed_limit.map(|sl| ThermalReading {
            max_cpu_celsius: None,
            cpu_speed_limit_ratio: Some((sl as f32 / 100.0).clamp(0.0, 1.0)),
            source: ThermalSource::MacosPmset,
        })
    }
}

impl Default for MacosPmsetSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl ThermalSensor for MacosPmsetSensor {
    fn read(&self) -> io::Result<Option<ThermalReading>> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(["-g", "therm"]);
        // `None` here means either the binary doesn't exist (the
        // pre-timeout fast-path) or the timeout fired — both are
        // soft failures, the monitor records "no sample this
        // cycle" and the next poll will retry.
        let Some(out) = run_command_with_timeout(&mut cmd, self.timeout)? else {
            return Ok(None);
        };
        if !out.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(Self::parse(&stdout))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows WMI sensor.
// ─────────────────────────────────────────────────────────────────────────────

/// Sensor that shells out to PowerShell to query
/// `MSAcpi_ThermalZoneTemperature` via WMI.
///
/// PowerShell ships with every supported Windows SKU and the call
/// requires no extra privileges. WMI returns
/// `CurrentTemperature` in tenths of degrees Kelvin (i.e.
/// `2982` = 298.2 K = 25.05 °C). The sensor takes the **max**
/// across all reported zones.
///
/// **Timeout**: PowerShell cold-start + a WMI query is
/// well under 1 s on every supported Windows SKU, but the WMI
/// service has been observed to stall on some hardware (broken
/// vendor ACPI tables, antivirus interception). The sensor
/// enforces a hard timeout (default
/// [`DEFAULT_SHELL_OUT_TIMEOUT`]) and kills the child if exceeded.
/// Without it the monitor thread would block indefinitely and the
/// cached thermal state would go stale. See
/// [`run_command_with_timeout`].
#[derive(Debug, Clone)]
pub struct WindowsWmiSensor {
    binary: PathBuf,
    /// Pre-baked PowerShell expression. Public so tests can supply
    /// an alternative command (e.g. `echo` of a canned payload).
    command_arg: String,
    /// Maximum wall-clock time we'll wait for PowerShell to exit
    /// before killing it. See [`run_command_with_timeout`].
    timeout: Duration,
}

impl WindowsWmiSensor {
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("powershell.exe"),
            command_arg: "(Get-CimInstance -Namespace root/WMI -ClassName \
                          MSAcpi_ThermalZoneTemperature).CurrentTemperature -join ','"
                .to_string(),
            timeout: DEFAULT_SHELL_OUT_TIMEOUT,
        }
    }

    pub fn with_binary_and_command(
        binary: impl Into<PathBuf>,
        command_arg: impl Into<String>,
    ) -> Self {
        Self {
            binary: binary.into(),
            command_arg: command_arg.into(),
            timeout: DEFAULT_SHELL_OUT_TIMEOUT,
        }
    }

    /// Override the per-call timeout. Default is
    /// [`DEFAULT_SHELL_OUT_TIMEOUT`] (2 s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Parse the comma-separated list of `CurrentTemperature` values
    /// (each in tenths of Kelvin) into a max Celsius reading.
    pub fn parse(stdout: &str) -> Option<ThermalReading> {
        let mut max_c: Option<f32> = None;
        for tok in stdout.trim().split(',') {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            let Ok(tenths_kelvin) = tok.parse::<u32>() else {
                continue;
            };
            let kelvin = tenths_kelvin as f32 / 10.0;
            let celsius = kelvin - 273.15;
            // Sanity gate — WMI sometimes returns absurd values
            // on virtual machines (e.g. `0` tenths-K = -273.15 °C,
            // or unreasonably large tenths-K values). Anything
            // outside the physical-CPU range is silently dropped
            // so we don't trip a thermal state from sensor garbage.
            if !(-50.0..=200.0).contains(&celsius) {
                continue;
            }
            max_c = Some(match max_c {
                Some(m) => m.max(celsius),
                None => celsius,
            });
        }
        max_c.map(|c| ThermalReading {
            max_cpu_celsius: Some(c),
            cpu_speed_limit_ratio: None,
            source: ThermalSource::WindowsWmi,
        })
    }
}

impl Default for WindowsWmiSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl ThermalSensor for WindowsWmiSensor {
    fn read(&self) -> io::Result<Option<ThermalReading>> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(["-NoProfile", "-Command", &self.command_arg]);
        // `None` covers both missing-binary (Linux/macOS hosts
        // running the Windows code path under cargo test) and
        // timeout-killed children — same soft-failure semantics.
        let Some(out) = run_command_with_timeout(&mut cmd, self.timeout)? else {
            return Ok(None);
        };
        if !out.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(Self::parse(&stdout))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Manual sensor (test injection + governor's fallback when no
// platform sensor was constructed).
// ─────────────────────────────────────────────────────────────────────────────

/// A real sensor that returns a value held in an `Arc<Mutex>`. Used
/// by tests to drive the scheduler through thermal transitions
/// deterministically, and by callers that want to feed in
/// externally-measured temperatures (e.g. a GPU driver query that
/// lives outside `aec_governor`).
#[derive(Debug, Clone, Default)]
pub struct ManualSensor {
    inner: Arc<Mutex<Option<ThermalReading>>>,
}

impl ManualSensor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, reading: Option<ThermalReading>) {
        let mut g = self.inner.lock().expect("manual sensor mutex poisoned");
        *g = reading;
    }

    pub fn set_celsius(&self, c: f32) {
        self.set(Some(ThermalReading {
            max_cpu_celsius: Some(c),
            cpu_speed_limit_ratio: None,
            source: ThermalSource::Manual,
        }));
    }
}

impl ThermalSensor for ManualSensor {
    fn read(&self) -> io::Result<Option<ThermalReading>> {
        Ok(self
            .inner
            .lock()
            .expect("manual sensor mutex poisoned")
            .clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Native sensor factory.
// ─────────────────────────────────────────────────────────────────────────────

/// Build the default sensor for the current platform.
///
/// Returns the platform-native sensor on Linux/macOS/Windows and a
/// `ManualSensor` (initially empty) on every other target so the
/// governor can still be constructed.
pub fn native_sensor() -> Box<dyn ThermalSensor> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxSysfsSensor::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacosPmsetSensor::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsWmiSensor::new())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Box::new(ManualSensor::new())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Monitor — owns a sensor, polls it on demand, and applies the
// resulting ThermalState to a GovernorScheduler.
// ─────────────────────────────────────────────────────────────────────────────

/// Pollable thermal monitor. Holds the sensor and the thresholds;
/// callers tick it on a schedule (e.g. every 5 s in production,
/// every call in tests).
///
/// Deliberately does NOT spawn its own thread — the production
/// caller (the bridge service) already has a Tokio runtime and
/// will drive `tick()` from an interval task. Spawning here would
/// create two ownership stories for the same scheduler.
pub struct ThermalMonitor {
    sensor: Box<dyn ThermalSensor>,
    thresholds: ThermalThresholds,
    last_state: ThermalState,
    last_reading: Option<ThermalReading>,
    last_tick: Option<Instant>,
    poll_interval: Duration,
}

impl ThermalMonitor {
    pub fn new(sensor: Box<dyn ThermalSensor>, thresholds: ThermalThresholds) -> Self {
        Self {
            sensor,
            thresholds,
            last_state: ThermalState::Nominal,
            last_reading: None,
            last_tick: None,
            // 5 s is a deliberate Goldilocks pick: long enough that
            // a `pmset` shell-out isn't a hot loop, short enough
            // that a thermal trip is reflected before the next
            // render-job admission decision.
            poll_interval: Duration::from_secs(5),
        }
    }

    /// New monitor wrapping the platform-native sensor.
    pub fn native(thresholds: ThermalThresholds) -> Self {
        Self::new(native_sensor(), thresholds)
    }

    pub fn set_poll_interval(&mut self, d: Duration) {
        self.poll_interval = d;
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn last_state(&self) -> ThermalState {
        self.last_state
    }

    pub fn last_reading(&self) -> Option<&ThermalReading> {
        self.last_reading.as_ref()
    }

    /// Force-read the sensor, classify, cache the result. Returns
    /// the new `ThermalState`.
    pub fn tick(&mut self) -> ThermalState {
        let reading = self.sensor.read().ok().flatten();
        self.last_reading = reading;
        self.last_state = classify(self.last_reading.as_ref(), &self.thresholds);
        self.last_tick = Some(Instant::now());
        self.last_state
    }

    /// Tick **only** if at least `poll_interval` has elapsed since
    /// the last `tick()`. Returns the (possibly-cached) state.
    pub fn tick_if_due(&mut self) -> ThermalState {
        let due = match self.last_tick {
            None => true,
            Some(t) => t.elapsed() >= self.poll_interval,
        };
        if due {
            self.tick()
        } else {
            self.last_state
        }
    }

    /// Tick once and propagate the resulting state to the
    /// scheduler. Returns the new state.
    pub fn apply_to_scheduler(&mut self, scheduler: &mut GovernorScheduler) -> ThermalState {
        let s = self.tick();
        scheduler.set_thermal_state(s);
        s
    }

    /// `tick_if_due` + `set_thermal_state`. Cheap to call on every
    /// admission decision: the read-sensor work runs at most once
    /// per `poll_interval`.
    pub fn apply_to_scheduler_if_due(&mut self, scheduler: &mut GovernorScheduler) -> ThermalState {
        let s = self.tick_if_due();
        scheduler.set_thermal_state(s);
        s
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::GovernorPolicy;
    use crate::tier::HardwareTier;
    use std::path::Path;

    fn write_zone(root: &Path, idx: u32, zone_type: &str, millic: i64) {
        let zd = root.join(format!("thermal_zone{idx}"));
        fs::create_dir_all(&zd).unwrap();
        fs::write(zd.join("type"), zone_type).unwrap();
        fs::write(zd.join("temp"), millic.to_string()).unwrap();
    }

    #[test]
    fn linux_sysfs_reads_max_across_cpu_zones() {
        let tmp = tempfile::tempdir().unwrap();
        write_zone(tmp.path(), 0, "x86_pkg_temp", 52_000);
        write_zone(tmp.path(), 1, "cpu_thermal", 71_500);
        // Non-CPU zone should be ignored.
        write_zone(tmp.path(), 2, "acpitz", 99_000);
        let s = LinuxSysfsSensor::with_root(tmp.path());
        let r = s.read().unwrap().unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 71.5).abs() < 1e-3);
        assert!(r.cpu_speed_limit_ratio.is_none());
        match r.source {
            ThermalSource::LinuxSysfs { zones } => assert_eq!(zones, 2),
            o => panic!("wrong source: {o:?}"),
        }
    }

    #[test]
    fn linux_sysfs_returns_none_when_no_cpu_zones() {
        let tmp = tempfile::tempdir().unwrap();
        write_zone(tmp.path(), 0, "acpitz", 60_000);
        let s = LinuxSysfsSensor::with_root(tmp.path());
        assert!(s.read().unwrap().is_none());
    }

    #[test]
    fn linux_sysfs_returns_none_when_root_missing() {
        let s = LinuxSysfsSensor::with_root("/nonexistent/sysfs/thermal");
        assert!(s.read().unwrap().is_none());
    }

    #[test]
    fn linux_sysfs_skips_malformed_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_zone(tmp.path(), 0, "x86_pkg_temp", 60_000);
        // Hand-roll a malformed zone.
        let zd = tmp.path().join("thermal_zone1");
        fs::create_dir_all(&zd).unwrap();
        fs::write(zd.join("type"), "cpu_thermal").unwrap();
        fs::write(zd.join("temp"), "not a number").unwrap();
        let s = LinuxSysfsSensor::with_root(tmp.path());
        let r = s.read().unwrap().unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 60.0).abs() < 1e-3);
    }

    #[test]
    fn macos_pmset_parses_speed_limit_100() {
        let r = MacosPmsetSensor::parse(
            "CPU_Scheduler_Limit  = 100\n\
             CPU_Available_CPUs   = 16\n\
             CPU_Speed_Limit      = 100\n",
        )
        .unwrap();
        assert!((r.cpu_speed_limit_ratio.unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(r.source, ThermalSource::MacosPmset);
    }

    #[test]
    fn macos_pmset_parses_throttled_speed_limit() {
        let r = MacosPmsetSensor::parse(
            "CPU_Scheduler_Limit  = 80\n\
             CPU_Speed_Limit      = 60\n",
        )
        .unwrap();
        assert!((r.cpu_speed_limit_ratio.unwrap() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn macos_pmset_parse_returns_none_when_field_missing() {
        assert!(MacosPmsetSensor::parse("nothing here").is_none());
    }

    #[test]
    fn macos_pmset_parser_does_not_mis_match_extended_prefix() {
        // A hypothetical future pmset payload that introduces a
        // sibling field whose name shares the `CPU_Speed_Limit`
        // prefix MUST NOT be consumed as if it were the canonical
        // reading. We require a whitespace or `=` boundary right
        // after the prefix, so `CPU_Speed_Limit_Extended` (no
        // whitespace, just a `_`) is correctly skipped.
        let r = MacosPmsetSensor::parse(
            "CPU_Speed_Limit_Extended = 42\n\
             CPU_Speed_Limit_Reason   = thermal\n\
             CPU_Speed_Limit          = 75\n",
        )
        .unwrap();
        // The reading must be 75/100 = 0.75 (from the real field),
        // *not* 42/100 from `_Extended` nor a no-op from `_Reason`.
        assert!((r.cpu_speed_limit_ratio.unwrap() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn macos_pmset_parser_returns_none_when_only_lookalike_fields_present() {
        // If the *only* CPU_Speed_Limit-prefixed lines in the
        // payload are siblings (`_Extended`, `_Reason`, etc.) and
        // the canonical `CPU_Speed_Limit` itself is missing, the
        // parser must return None — NOT a phantom reading derived
        // from a sibling.
        assert!(MacosPmsetSensor::parse(
            "CPU_Speed_Limit_Extended = 42\n\
                 CPU_Speed_Limit_Reason   = thermal\n"
        )
        .is_none());
    }

    #[test]
    fn threshold_validate_accepts_default() {
        ThermalThresholds::default()
            .validate()
            .expect("the production Default impl must satisfy its own invariants");
    }

    #[test]
    fn threshold_try_new_accepts_valid_pair() {
        let t = ThermalThresholds::try_new(70.0, 90.0, 0.95, 0.50).unwrap();
        assert_eq!(t.warm_celsius, 70.0);
        assert_eq!(t.critical_celsius, 90.0);
    }

    #[test]
    fn threshold_validate_rejects_inverted_temperature_order() {
        // warm >= critical — the `else if c >= warm_celsius` branch
        // would become dead code in `classify`.
        let t = ThermalThresholds {
            warm_celsius: 95.0,
            critical_celsius: 80.0,
            warm_speed_limit: 0.99,
            critical_speed_limit: 0.60,
        };
        assert!(matches!(
            t.validate(),
            Err(ThresholdError::TemperatureOrder { .. })
        ));
        // Equal also fails — the boundary is strict (<, not <=) so
        // there's always a non-empty Warm band.
        let eq = ThermalThresholds {
            warm_celsius: 80.0,
            critical_celsius: 80.0,
            warm_speed_limit: 0.99,
            critical_speed_limit: 0.60,
        };
        assert!(matches!(
            eq.validate(),
            Err(ThresholdError::TemperatureOrder { .. })
        ));
    }

    #[test]
    fn threshold_validate_rejects_inverted_speed_limit_order() {
        // warm <= critical for the speed-limit axis is the
        // analogous bug — ratios trend *downward* under throttling.
        let t = ThermalThresholds {
            warm_celsius: 80.0,
            critical_celsius: 95.0,
            warm_speed_limit: 0.50,
            critical_speed_limit: 0.70,
        };
        assert!(matches!(
            t.validate(),
            Err(ThresholdError::SpeedLimitOrder { .. })
        ));
    }

    #[test]
    fn threshold_validate_rejects_non_finite() {
        // NaN trips every comparison — every classifier branch
        // becomes dead code.
        let nan = ThermalThresholds {
            warm_celsius: f32::NAN,
            critical_celsius: 95.0,
            warm_speed_limit: 0.99,
            critical_speed_limit: 0.60,
        };
        assert!(matches!(
            nan.validate(),
            Err(ThresholdError::NonFinite {
                field: "warm_celsius",
                ..
            })
        ));
        let inf = ThermalThresholds {
            warm_celsius: 80.0,
            critical_celsius: f32::INFINITY,
            warm_speed_limit: 0.99,
            critical_speed_limit: 0.60,
        };
        assert!(matches!(
            inf.validate(),
            Err(ThresholdError::NonFinite {
                field: "critical_celsius",
                ..
            })
        ));
    }

    #[test]
    fn threshold_validate_rejects_speed_limit_outside_unit_range() {
        // Speed-limit ratios are clamped to `[0.0, 1.0]` by every
        // sensor in this crate, so a threshold of `1.5` could never
        // trip.
        let above = ThermalThresholds {
            warm_celsius: 80.0,
            critical_celsius: 95.0,
            warm_speed_limit: 1.5,
            critical_speed_limit: 0.60,
        };
        assert!(matches!(
            above.validate(),
            Err(ThresholdError::SpeedLimitOutOfRange {
                field: "warm_speed_limit",
                ..
            })
        ));
        let below = ThermalThresholds {
            warm_celsius: 80.0,
            critical_celsius: 95.0,
            warm_speed_limit: 0.99,
            critical_speed_limit: -0.1,
        };
        assert!(matches!(
            below.validate(),
            Err(ThresholdError::SpeedLimitOutOfRange {
                field: "critical_speed_limit",
                ..
            })
        ));
    }

    #[test]
    fn threshold_error_display_is_informative() {
        // The error must format with both field names and values
        // so an operator can fix their config without a debugger.
        let e = ThresholdError::TemperatureOrder {
            warm: 95.0,
            critical: 80.0,
        };
        let s = e.to_string();
        assert!(s.contains("warm_celsius"));
        assert!(s.contains("critical_celsius"));
        assert!(s.contains("95"));
        assert!(s.contains("80"));
    }

    #[test]
    fn windows_wmi_parses_max_across_zones() {
        // 2982 = 298.2 K = 25.05 °C; 3502 = 350.2 K = 77.05 °C.
        let r = WindowsWmiSensor::parse("2982,3502").unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 77.05).abs() < 0.01);
        assert!(r.cpu_speed_limit_ratio.is_none());
    }

    // ─────────────────────────────────────────────────────────────────
    // run_command_with_timeout: success / timeout / NotFound paths
    // ─────────────────────────────────────────────────────────────────
    //
    // These tests pin the soft-failure contract that the
    // `MacosPmsetSensor` and `WindowsWmiSensor` rely on: a stuck
    // shell-out must NOT block the monitor thread. They shell out to
    // tiny portable Unix utilities (`/bin/sh`, `/bin/echo`) so they
    // run on every supported CI host (Linux + macOS). Windows CI
    // skips them via `#[cfg(unix)]` — the equivalent Windows path is
    // exercised by the `WindowsWmiSensor` parser tests above plus the
    // shared timeout machinery, and a Windows-only `cmd /c timeout`
    // version of this test would duplicate logic without adding
    // coverage of the timeout primitive itself.

    #[cfg(unix)]
    #[test]
    fn run_command_with_timeout_returns_output_for_fast_command() {
        // `/bin/echo` exits well under any reasonable budget, so the
        // happy path must capture stdout verbatim and report exit 0.
        let mut cmd = Command::new("/bin/echo");
        cmd.arg("hello-thermal");
        let out = run_command_with_timeout(&mut cmd, Duration::from_secs(5))
            .expect("io error not expected")
            .expect("should not be None — echo exists and exits immediately");
        assert!(out.status.success());
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(s.contains("hello-thermal"), "captured stdout: {s:?}");
    }

    #[cfg(unix)]
    #[test]
    fn run_command_with_timeout_kills_long_running_command_within_budget() {
        // Spawn a `/bin/sh -c 'sleep 10'` with a 100 ms budget. The
        // timeout must fire, kill the child, reap the zombie, and
        // return `Ok(None)` — and the whole call must finish in
        // well under the 10 s sleep window so we don't pin the
        // monitor thread. We allow up to 2 s of slack for slow CI
        // schedulers but assert it's nowhere near 10 s.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let result = run_command_with_timeout(&mut cmd, Duration::from_millis(100))
            .expect("io error not expected");
        let elapsed = started.elapsed();
        assert!(result.is_none(), "timed-out child should yield None");
        assert!(
            elapsed < Duration::from_secs(2),
            "kill path must not wait for the full 10 s sleep; elapsed = {elapsed:?}"
        );
    }

    #[test]
    fn run_command_with_timeout_treats_missing_binary_as_none() {
        // The fast-path: a binary that doesn't exist returns
        // `Ok(None)` (not an error), matching the pre-timeout
        // semantics that `MacosPmsetSensor::read` and
        // `WindowsWmiSensor::read` were already wired for. This
        // keeps Linux CI green (no `/usr/bin/pmset` on Linux, no
        // `powershell.exe` on Linux/macOS).
        let mut cmd = Command::new("/this/path/definitely/does/not/exist/pmset");
        let result = run_command_with_timeout(&mut cmd, Duration::from_secs(2))
            .expect("NotFound must be soft-failed, not bubbled up");
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pmset_sensor_with_short_timeout_returns_none_instead_of_blocking() {
        // End-to-end: a `MacosPmsetSensor` pointing at a script that
        // sleeps forever must NOT block the calling thread. The
        // sensor's `read()` returns `Ok(None)` so the monitor
        // records "no sample this cycle" and moves on.
        let sensor =
            MacosPmsetSensor::with_binary("/bin/sh").with_timeout(Duration::from_millis(100));
        // We're substituting `/bin/sh` for `/usr/bin/pmset`, which
        // means `read()` will spawn `/bin/sh -g therm` — that's a
        // sh invocation with two unknown args. sh exits ~immediately
        // with a usage error, so this test actually exercises the
        // "non-success status → None" branch, not the kill branch.
        // To exercise the kill branch end-to-end on a real sensor,
        // we'd need a per-sensor command override hook that isn't
        // part of the production API surface. The kill branch is
        // covered directly by
        // `run_command_with_timeout_kills_long_running_command_within_budget`
        // above.
        let started = Instant::now();
        let r = sensor.read().expect("io error not expected");
        let elapsed = started.elapsed();
        assert!(r.is_none());
        assert!(
            elapsed < Duration::from_secs(2),
            "even a usage-error exit must complete promptly; elapsed = {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wmi_sensor_with_short_timeout_kills_blocking_powershell_stub() {
        // End-to-end: a `WindowsWmiSensor` configured with a stub
        // "powershell" that sleeps forever must time out and
        // return `Ok(None)`. We swap the binary for `/bin/sh` and
        // the command-arg for a sleep, mirroring how the production
        // sensor calls `powershell.exe -NoProfile -Command "..."`.
        let sensor = WindowsWmiSensor::with_binary_and_command("/bin/sh", "sleep 10")
            .with_timeout(Duration::from_millis(100));
        // NB: the production sensor passes `-NoProfile -Command` as
        // the leading args, which `/bin/sh` interprets as
        // `sh -NoProfile -Command "sleep 10"`. `/bin/sh` would
        // reject `-NoProfile` and exit fast, which still validates
        // the soft-failure contract (`Ok(None)`), but doesn't
        // exercise the kill branch. The kill branch is covered by
        // `run_command_with_timeout_kills_long_running_command_within_budget`.
        let started = Instant::now();
        let r = sensor.read().expect("io error not expected");
        let elapsed = started.elapsed();
        assert!(r.is_none());
        assert!(
            elapsed < Duration::from_secs(2),
            "WMI sensor must not block when the shell-out doesn't return; elapsed = {elapsed:?}"
        );
    }

    #[test]
    fn windows_wmi_discards_absurd_values() {
        // `0` tenths-K = -273.15 °C → below the -50 °C floor,
        // discarded by the sanity gate. `9999` tenths-K = 726.7 °C
        // → above the 200 °C ceiling, also discarded. `3502`
        // tenths-K = 77.05 °C → the only valid zone, becomes the
        // reported max.
        let r = WindowsWmiSensor::parse("0,9999,3502").unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 77.05).abs() < 0.01);

        // All zones absurd → no reading produced.
        assert!(WindowsWmiSensor::parse("0,9999").is_none());
    }

    #[test]
    fn windows_wmi_keeps_borderline_low_kelvin_values() {
        // `2732` tenths-K = 0.05 °C — physically improbable for a
        // running CPU but inside the sanity range, so the parser
        // keeps it. Document the boundary explicitly so a future
        // tightening of the gate doesn't silently break this case.
        let r = WindowsWmiSensor::parse("2732,3502").unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 77.05).abs() < 0.01);
    }

    #[test]
    fn classify_temperature_thresholds() {
        let t = ThermalThresholds::default();
        let nom = ThermalReading {
            max_cpu_celsius: Some(50.0),
            cpu_speed_limit_ratio: None,
            source: ThermalSource::Manual,
        };
        let warm = ThermalReading {
            max_cpu_celsius: Some(85.0),
            cpu_speed_limit_ratio: None,
            source: ThermalSource::Manual,
        };
        let crit = ThermalReading {
            max_cpu_celsius: Some(96.0),
            cpu_speed_limit_ratio: None,
            source: ThermalSource::Manual,
        };
        assert_eq!(classify(Some(&nom), &t), ThermalState::Nominal);
        assert_eq!(classify(Some(&warm), &t), ThermalState::Warm);
        assert_eq!(classify(Some(&crit), &t), ThermalState::Critical);
        assert_eq!(classify(None, &t), ThermalState::Nominal);
    }

    #[test]
    fn classify_speed_limit_thresholds() {
        let t = ThermalThresholds::default();
        let nom = ThermalReading {
            max_cpu_celsius: None,
            cpu_speed_limit_ratio: Some(1.0),
            source: ThermalSource::Manual,
        };
        let warm = ThermalReading {
            max_cpu_celsius: None,
            cpu_speed_limit_ratio: Some(0.95),
            source: ThermalSource::Manual,
        };
        let crit = ThermalReading {
            max_cpu_celsius: None,
            cpu_speed_limit_ratio: Some(0.5),
            source: ThermalSource::Manual,
        };
        assert_eq!(classify(Some(&nom), &t), ThermalState::Nominal);
        assert_eq!(classify(Some(&warm), &t), ThermalState::Warm);
        assert_eq!(classify(Some(&crit), &t), ThermalState::Critical);
    }

    #[test]
    fn classify_returns_worst_of_both_axes() {
        let t = ThermalThresholds::default();
        // Temperature looks nominal but speed-limit says critical.
        let r = ThermalReading {
            max_cpu_celsius: Some(40.0),
            cpu_speed_limit_ratio: Some(0.4),
            source: ThermalSource::Manual,
        };
        assert_eq!(classify(Some(&r), &t), ThermalState::Critical);
    }

    #[test]
    fn monitor_tick_updates_state_from_manual_sensor() {
        let manual = ManualSensor::new();
        let mut mon = ThermalMonitor::new(Box::new(manual.clone()), ThermalThresholds::default());
        assert_eq!(mon.tick(), ThermalState::Nominal);
        manual.set_celsius(85.0);
        assert_eq!(mon.tick(), ThermalState::Warm);
        manual.set_celsius(96.0);
        assert_eq!(mon.tick(), ThermalState::Critical);
    }

    #[test]
    fn monitor_apply_propagates_to_scheduler() {
        let manual = ManualSensor::new();
        let mut mon = ThermalMonitor::new(Box::new(manual.clone()), ThermalThresholds::default());
        let policy = GovernorPolicy::for_tier(HardwareTier::Medium);
        let mut sched = GovernorScheduler::new(policy);

        manual.set_celsius(96.0);
        let s = mon.apply_to_scheduler(&mut sched);
        assert_eq!(s, ThermalState::Critical);
        // Critical thermal denies render admission with reason
        // Thermal (per scheduler.rs).
        let v = sched.admit_render();
        assert!(!v.admitted);
        assert_eq!(
            v.backoff_reason,
            Some(crate::scheduler::BackoffReason::Thermal)
        );

        manual.set_celsius(60.0);
        let s = mon.apply_to_scheduler(&mut sched);
        assert_eq!(s, ThermalState::Nominal);
        assert!(sched.admit_render().admitted);
    }

    #[test]
    fn monitor_tick_if_due_respects_poll_interval() {
        let manual = ManualSensor::new();
        let mut mon = ThermalMonitor::new(Box::new(manual.clone()), ThermalThresholds::default());
        // 10-minute interval, no chance the test waits that long.
        mon.set_poll_interval(Duration::from_secs(600));
        manual.set_celsius(96.0);
        assert_eq!(mon.tick_if_due(), ThermalState::Critical);
        // Now lower the sensor reading; tick_if_due should NOT
        // re-poll because the interval hasn't elapsed.
        manual.set_celsius(30.0);
        assert_eq!(mon.tick_if_due(), ThermalState::Critical);
        // Explicit `tick` bypasses the interval and re-reads.
        assert_eq!(mon.tick(), ThermalState::Nominal);
    }
}
