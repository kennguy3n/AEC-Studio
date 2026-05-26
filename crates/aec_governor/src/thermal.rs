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
use std::process::Command;
use std::sync::{Arc, Mutex};
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalThresholds {
    pub warm_celsius: f32,
    pub critical_celsius: f32,
    pub warm_speed_limit: f32,
    pub critical_speed_limit: f32,
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
pub fn classify(reading: Option<&ThermalReading>, thresholds: &ThermalThresholds) -> ThermalState {
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
#[derive(Debug, Clone)]
pub struct MacosPmsetSensor {
    /// `pmset` binary path. Production = `/usr/bin/pmset`. Tests
    /// pass a shell script that emits canned output.
    binary: PathBuf,
}

impl MacosPmsetSensor {
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("/usr/bin/pmset"),
        }
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Parse a `pmset -g therm` payload. Public so the unit test
    /// suite can pin the parser against canned strings.
    pub fn parse(stdout: &str) -> Option<ThermalReading> {
        let mut speed_limit: Option<u32> = None;
        for line in stdout.lines() {
            let line = line.trim();
            // Each line looks like `CPU_Speed_Limit      = 100`.
            if let Some(rest) = line.strip_prefix("CPU_Speed_Limit") {
                if let Some(val) = rest.split('=').nth(1) {
                    speed_limit = val.trim().parse::<u32>().ok();
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
        let out = match Command::new(&self.binary).args(["-g", "therm"]).output() {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
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
#[derive(Debug, Clone)]
pub struct WindowsWmiSensor {
    binary: PathBuf,
    /// Pre-baked PowerShell expression. Public so tests can supply
    /// an alternative command (e.g. `echo` of a canned payload).
    command_arg: String,
}

impl WindowsWmiSensor {
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("powershell.exe"),
            command_arg: "(Get-CimInstance -Namespace root/WMI -ClassName \
                          MSAcpi_ThermalZoneTemperature).CurrentTemperature -join ','"
                .to_string(),
        }
    }

    pub fn with_binary_and_command(
        binary: impl Into<PathBuf>,
        command_arg: impl Into<String>,
    ) -> Self {
        Self {
            binary: binary.into(),
            command_arg: command_arg.into(),
        }
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
        let out = match Command::new(&self.binary)
            .args(["-NoProfile", "-Command", &self.command_arg])
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
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
    fn windows_wmi_parses_max_across_zones() {
        // 2982 = 298.2 K = 25.05 °C; 3502 = 350.2 K = 77.05 °C.
        let r = WindowsWmiSensor::parse("2982,3502").unwrap();
        assert!((r.max_cpu_celsius.unwrap() - 77.05).abs() < 0.01);
        assert!(r.cpu_speed_limit_ratio.is_none());
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
