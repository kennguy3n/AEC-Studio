//! Phase 12 Task 27 — memory pressure monitor + eviction dispatcher.
//!
//! The governor needs to react when the AEC Studio process is about to
//! run out of RAM. The two failure modes we guard against are:
//!
//! * **Mesh cache bloat** — large tessellated geometry caches build up
//!   across long modelling sessions.
//! * **Snapshot cache bloat** — undo/redo snapshots accumulate when the
//!   user runs many commands between save points.
//!
//! Both are addressable by *evicting* the cache LRU; the scheduler
//! additionally reduces path-tracer tile concurrency once memory is
//! pressured so a single render job can't push the process over.
//!
//! This module is intentionally pull-based: callers (the bridge, a UI
//! status tick) drive [`MemoryMonitor::sample`] on a 5s cadence, which
//! refreshes the underlying `sysinfo::System`, computes the pressure
//! state, and fires listeners. Tests inject a custom
//! [`MemorySampler`] so the code path stays deterministic in CI.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Snapshot of system memory at a moment in time, in megabytes. RSS is
/// reported separately because we care about the *AEC Studio process*
/// rather than the system as a whole — a 64 GB workstation that runs a
/// 30 GB browser is *not* the same scenario as Studio itself sitting at
/// 30 GB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySample {
    pub total_mb: u64,
    pub used_mb: u64,
    pub process_rss_mb: u64,
}

impl MemorySample {
    /// Fraction of system RAM in use by *this process*, in `[0.0, 1.0]`.
    pub fn process_fraction(&self) -> f32 {
        if self.total_mb == 0 {
            return 0.0;
        }
        (self.process_rss_mb as f32 / self.total_mb as f32).clamp(0.0, 1.0)
    }

    /// Fraction of system RAM in use system-wide, in `[0.0, 1.0]`.
    pub fn system_fraction(&self) -> f32 {
        if self.total_mb == 0 {
            return 0.0;
        }
        (self.used_mb as f32 / self.total_mb as f32).clamp(0.0, 1.0)
    }
}

/// Coarse memory pressure level surfaced to the StatusBar and consumed
/// by the scheduler. The thresholds are calibrated against the
/// 75 %-of-system-RAM target from the Phase 12 spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    /// Process RSS < `pressured_fraction` of system RAM. No action.
    Normal,
    /// Process RSS ≥ `pressured_fraction` (75 % by default). Caches
    /// should evict LRU entries; the scheduler reduces tile
    /// concurrency.
    Pressured,
    /// Process RSS ≥ `critical_fraction` (90 % by default). Caches
    /// should evict aggressively; render admission is denied with
    /// [`crate::BackoffReason::MemoryPressure`].
    Critical,
}

impl MemoryState {
    /// Returns true when we should be evicting / throttling.
    pub fn requires_action(self) -> bool {
        !matches!(self, MemoryState::Normal)
    }
}

/// Thresholds that drive [`MemoryState`] classification. Defaults
/// follow the Phase 12 spec (75 % pressured, 90 % critical).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemoryThresholds {
    pub pressured_fraction: f32,
    pub critical_fraction: f32,
}

impl Default for MemoryThresholds {
    fn default() -> Self {
        Self {
            pressured_fraction: 0.75,
            critical_fraction: 0.90,
        }
    }
}

impl MemoryThresholds {
    pub fn classify(self, sample: &MemorySample) -> MemoryState {
        let frac = sample.process_fraction();
        if frac >= self.critical_fraction {
            MemoryState::Critical
        } else if frac >= self.pressured_fraction {
            MemoryState::Pressured
        } else {
            MemoryState::Normal
        }
    }
}

/// Trait implemented by subsystems that hold evictable state (mesh
/// caches, snapshot caches, etc.). Listeners are invoked on every
/// pressure transition into [`MemoryState::Pressured`] or
/// [`MemoryState::Critical`].
pub trait MemoryPressureListener: Send + Sync {
    /// Drop the oldest evictable entries from the cache. The listener
    /// is free to use its own LRU heuristics — the governor only
    /// signals *that* pressure exists, not *how much* to drop.
    fn on_memory_pressure(&self, state: MemoryState);
}

/// Pluggable sampler — production wires [`SysinfoSampler`], tests
/// inject [`FakeSampler`] for deterministic state transitions.
pub trait MemorySampler: Send + Sync {
    fn sample(&mut self) -> MemorySample;
}

/// Production sampler backed by the `sysinfo` crate. Refreshes the
/// process list and the global memory counters on each call.
pub struct SysinfoSampler {
    system: sysinfo::System,
    pid: sysinfo::Pid,
}

impl Default for SysinfoSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoSampler {
    pub fn new() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        // sysinfo's current_pid() can return Err when run on an
        // unsupported platform — fall back to PID 0 which simply
        // produces an RSS reading of 0 (i.e. system-only metrics).
        let pid = sysinfo::get_current_pid().unwrap_or(sysinfo::Pid::from(0));
        Self { system, pid }
    }
}

impl MemorySampler for SysinfoSampler {
    fn sample(&mut self) -> MemorySample {
        // `refresh_memory` updates the global counters; we additionally
        // refresh just this process so RSS reflects the latest growth.
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            sysinfo::ProcessRefreshKind::new().with_memory(),
        );
        let total_mb = self.system.total_memory() / (1024 * 1024);
        let used_mb = self.system.used_memory() / (1024 * 1024);
        let process_rss_mb = self
            .system
            .process(self.pid)
            .map_or(0, |p| p.memory() / (1024 * 1024));
        MemorySample {
            total_mb,
            used_mb,
            process_rss_mb,
        }
    }
}

/// Test sampler — returns a queue of samples one at a time, falling
/// back to the last sample when exhausted so a long-running test
/// doesn't underflow.
pub struct FakeSampler {
    samples: Vec<MemorySample>,
    cursor: usize,
}

impl FakeSampler {
    pub fn new(samples: Vec<MemorySample>) -> Self {
        Self { samples, cursor: 0 }
    }
}

impl MemorySampler for FakeSampler {
    fn sample(&mut self) -> MemorySample {
        let idx = self.cursor.min(self.samples.len().saturating_sub(1));
        self.cursor += 1;
        self.samples.get(idx).copied().unwrap_or(MemorySample {
            total_mb: 16384,
            used_mb: 4096,
            process_rss_mb: 1024,
        })
    }
}

/// Drives memory pressure sampling and dispatches eviction callbacks.
///
/// Owns the sampler, the thresholds, and a registry of listeners.
/// Listener invocations are *de-duplicated* on transition — a listener
/// only sees a callback when the pressure level steps *up* (Normal →
/// Pressured, Pressured → Critical, or Normal → Critical directly).
/// This avoids hammering the mesh cache with eviction calls every 5 s
/// while the process sits at 78 % RSS.
///
/// # Thread model
///
/// `MemoryMonitor` is a **single-writer** type. Both
/// [`Self::add_listener`] and [`Self::sample`] take `&mut self`, so
/// callers must serialise access — in practice this is the governor's
/// 5-second tick thread, which is the only writer in the process.
/// Listeners are dispatched without holding any lock because:
///
/// 1. The writer side already has exclusive access via the `&mut self`
///    borrow, so iterating over `&self.listeners` after pushing a new
///    state cannot race with another `add_listener` or `sample` call.
/// 2. Listener implementations are pure functions of the
///    `MemoryState` argument — none of them re-enter the monitor or
///    mutate shared state outside their own caches.
/// 3. Other subsystems that want the latest band without driving
///    the tick read [`Self::state`] / [`Self::last_sample`]
///    (both `&self`) — these never race with `sample` because the
///    single-writer constraint already serialises everything.
///
/// If we ever expose multi-threaded *registration* (e.g. plugin
/// listeners spawned from worker threads), the right fix is to switch
/// `listeners` to `RwLock<Vec<…>>` with a read-guard scope around the
/// dispatch loop — not to retrofit a `Mutex` on the existing
/// single-threaded path.
pub struct MemoryMonitor {
    sampler: Box<dyn MemorySampler>,
    thresholds: MemoryThresholds,
    last_state: MemoryState,
    last_sample: Option<MemorySample>,
    listeners: Vec<Arc<dyn MemoryPressureListener>>,
}

impl MemoryMonitor {
    pub fn new(sampler: Box<dyn MemorySampler>, thresholds: MemoryThresholds) -> Self {
        Self {
            sampler,
            thresholds,
            last_state: MemoryState::Normal,
            last_sample: None,
            listeners: Vec::new(),
        }
    }

    /// Convenience constructor wiring the production sysinfo sampler.
    pub fn with_sysinfo() -> Self {
        Self::new(Box::new(SysinfoSampler::new()), MemoryThresholds::default())
    }

    /// Register a listener. The monitor stores a strong [`Arc`] so the
    /// listener stays alive for the monitor's lifetime; subsystems
    /// that want a weak binding should wrap their cache in a
    /// shim that internally holds a `Weak`.
    pub fn add_listener(&mut self, listener: Arc<dyn MemoryPressureListener>) {
        self.listeners.push(listener);
    }

    /// Most recently observed sample, if any.
    pub fn last_sample(&self) -> Option<MemorySample> {
        self.last_sample
    }

    /// Most recently classified state. Defaults to `Normal` before the
    /// first sample.
    pub fn state(&self) -> MemoryState {
        self.last_state
    }

    /// Take a new sample, reclassify, fire listeners on upward
    /// transitions, and return the resulting state.
    ///
    /// Returns `(new_state, transitioned)` so callers can update the
    /// StatusBar without having to compare with the previous tick
    /// themselves.
    pub fn sample(&mut self) -> (MemoryState, bool) {
        let sample = self.sampler.sample();
        let new_state = self.thresholds.classify(&sample);
        let transitioned = new_state != self.last_state;
        let escalation = transitioned && Self::is_escalation(self.last_state, new_state);
        self.last_state = new_state;
        self.last_sample = Some(sample);
        if escalation {
            for listener in &self.listeners {
                listener.on_memory_pressure(new_state);
            }
        }
        (new_state, transitioned)
    }

    fn is_escalation(prev: MemoryState, next: MemoryState) -> bool {
        // Order: Normal < Pressured < Critical
        let rank = |s: MemoryState| match s {
            MemoryState::Normal => 0,
            MemoryState::Pressured => 1,
            MemoryState::Critical => 2,
        };
        rank(next) > rank(prev)
    }
}

/// Atomic, lockless counter used by tests + integration code as a
/// stand-in cache that records how many eviction callbacks it has
/// received. Production caches implement [`MemoryPressureListener`]
/// directly on their own type.
#[derive(Default)]
pub struct EvictionCounter {
    pub count: Mutex<u32>,
    pub last_state: Mutex<Option<MemoryState>>,
}

impl MemoryPressureListener for EvictionCounter {
    fn on_memory_pressure(&self, state: MemoryState) {
        let mut c = self.count.lock().unwrap();
        *c += 1;
        *self.last_state.lock().unwrap() = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_at(total_mb: u64, rss_mb: u64) -> MemorySample {
        MemorySample {
            total_mb,
            used_mb: rss_mb,
            process_rss_mb: rss_mb,
        }
    }

    #[test]
    fn classify_uses_process_fraction_against_thresholds() {
        let t = MemoryThresholds::default();
        assert_eq!(t.classify(&sample_at(10_000, 5_000)), MemoryState::Normal);
        // 7,500 / 10,000 = 0.75 → pressured boundary.
        assert_eq!(
            t.classify(&sample_at(10_000, 7_500)),
            MemoryState::Pressured
        );
        // 9,000 / 10,000 = 0.90 → critical boundary.
        assert_eq!(t.classify(&sample_at(10_000, 9_000)), MemoryState::Critical);
    }

    #[test]
    fn process_fraction_clamps_when_total_is_zero() {
        // `sysinfo::total_memory` can return 0 in malformed-platform
        // edge cases — the monitor must not panic with a divide-by-zero.
        let s = sample_at(0, 100);
        assert_eq!(s.process_fraction(), 0.0);
    }

    #[test]
    fn monitor_fires_listener_on_upward_transition_only() {
        // Normal → Normal → Pressured → Pressured → Critical → Pressured
        let samples = vec![
            sample_at(10_000, 1_000),
            sample_at(10_000, 2_000),
            sample_at(10_000, 7_500),
            sample_at(10_000, 7_600),
            sample_at(10_000, 9_500),
            sample_at(10_000, 8_000),
        ];
        let sampler = Box::new(FakeSampler::new(samples));
        let mut monitor = MemoryMonitor::new(sampler, MemoryThresholds::default());
        let counter = Arc::new(EvictionCounter::default());
        monitor.add_listener(counter.clone() as Arc<dyn MemoryPressureListener>);

        let states: Vec<MemoryState> = (0..6).map(|_| monitor.sample().0).collect();
        assert_eq!(
            states,
            vec![
                MemoryState::Normal,
                MemoryState::Normal,
                MemoryState::Pressured,
                MemoryState::Pressured,
                MemoryState::Critical,
                MemoryState::Pressured,
            ]
        );
        // Listener fires twice (Normal→Pressured, Pressured→Critical),
        // NOT on the Critical→Pressured downgrade.
        let final_count = *counter.count.lock().unwrap();
        assert_eq!(
            final_count, 2,
            "listener must fire only on upward transitions"
        );
    }

    #[test]
    fn monitor_records_last_sample_after_first_tick() {
        let s = sample_at(10_000, 1_500);
        let sampler = Box::new(FakeSampler::new(vec![s]));
        let mut m = MemoryMonitor::new(sampler, MemoryThresholds::default());
        assert!(m.last_sample().is_none(), "no sample before tick");
        m.sample();
        assert_eq!(m.last_sample(), Some(s));
    }

    #[test]
    fn requires_action_is_true_for_pressured_and_critical() {
        assert!(!MemoryState::Normal.requires_action());
        assert!(MemoryState::Pressured.requires_action());
        assert!(MemoryState::Critical.requires_action());
    }
}
