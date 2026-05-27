//! Resource governor scheduler.
//!
//! Rate-limits render and AI jobs based on:
//!   - the active policy (concurrent jobs, parallel requests),
//!   - the current load (running jobs counter),
//!   - memory pressure (`available_ram_mb` falling below the cache budget),
//!   - thermal back-off (CPU/GPU temperature signal — supplied externally,
//!     since sysinfo's component support is platform-specific).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::policy::GovernorPolicy;
use crate::ui_report::ThermalState;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler is throttled: {0:?}")]
    Throttled(BackoffReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackoffReason {
    ConcurrencyLimit,
    MemoryPressure,
    Thermal,
    UserOverride,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleVerdict {
    pub admitted: bool,
    pub backoff_reason: Option<BackoffReason>,
}

#[derive(Debug, Clone)]
pub struct GovernorScheduler {
    policy: GovernorPolicy,
    running_render_jobs: u32,
    running_ai_jobs: u32,
    available_ram_mb: u64,
    thermal: ThermalState,
    user_paused: bool,
    /// Phase 12 Task 27 — current memory pressure state, fed from
    /// [`crate::MemoryMonitor`] sampling. `Pressured` halves the
    /// render concurrency cap; `Critical` denies all render
    /// admissions.
    memory: crate::memory::MemoryState,
}

impl GovernorScheduler {
    pub fn new(policy: GovernorPolicy) -> Self {
        Self {
            policy,
            running_render_jobs: 0,
            running_ai_jobs: 0,
            available_ram_mb: u64::MAX,
            thermal: ThermalState::Nominal,
            user_paused: false,
            memory: crate::memory::MemoryState::Normal,
        }
    }

    pub fn set_policy(&mut self, policy: GovernorPolicy) {
        self.policy = policy;
    }

    pub fn set_available_ram_mb(&mut self, mb: u64) {
        self.available_ram_mb = mb;
    }

    pub fn set_thermal_state(&mut self, t: ThermalState) {
        self.thermal = t;
    }

    /// Phase 12 Task 27 — record the latest memory pressure state.
    /// The caller (typically the bridge tick) wires this from
    /// [`crate::MemoryMonitor::sample`].
    pub fn set_memory_state(&mut self, m: crate::memory::MemoryState) {
        self.memory = m;
    }

    pub fn memory_state(&self) -> crate::memory::MemoryState {
        self.memory
    }

    pub fn set_user_paused(&mut self, paused: bool) {
        self.user_paused = paused;
    }

    pub fn running_render_jobs(&self) -> u32 {
        self.running_render_jobs
    }

    pub fn running_ai_jobs(&self) -> u32 {
        self.running_ai_jobs
    }

    /// Decide whether a new render job can run *now*. The caller commits the
    /// admission via [`commit_render_start`] when the worker spawns.
    pub fn admit_render(&self) -> ScheduleVerdict {
        if self.user_paused {
            return ScheduleVerdict::deny(BackoffReason::UserOverride);
        }
        if self.thermal == ThermalState::Critical {
            return ScheduleVerdict::deny(BackoffReason::Thermal);
        }
        // Phase 12 Task 27: critical memory pressure denies all new
        // render admissions, even ones that fit in the mesh cache
        // budget. Pressured is handled below by halving concurrency.
        if matches!(self.memory, crate::memory::MemoryState::Critical) {
            return ScheduleVerdict::deny(BackoffReason::MemoryPressure);
        }
        if self.available_ram_mb < self.policy.mesh_cache_budget_mb as u64 {
            return ScheduleVerdict::deny(BackoffReason::MemoryPressure);
        }
        let thermal_cap = match self.thermal {
            ThermalState::Nominal => self.policy.render.max_concurrent_jobs,
            ThermalState::Warm => 1.max(self.policy.render.max_concurrent_jobs.saturating_sub(1)),
            ThermalState::Critical => 0,
        };
        // Memory-aware concurrency cap. Pressured halves the cap (but
        // never below 1 so a user-initiated render still makes
        // progress); Normal applies the thermal cap as-is.
        let cap = match self.memory {
            crate::memory::MemoryState::Pressured => 1.max(thermal_cap / 2),
            crate::memory::MemoryState::Critical => 0,
            crate::memory::MemoryState::Normal => thermal_cap,
        };
        if self.running_render_jobs >= cap {
            return ScheduleVerdict::deny(BackoffReason::ConcurrencyLimit);
        }
        ScheduleVerdict::ok()
    }

    pub fn admit_ai(&self) -> ScheduleVerdict {
        if self.user_paused {
            return ScheduleVerdict::deny(BackoffReason::UserOverride);
        }
        if self.running_render_jobs > 0 && !self.policy.render.allow_background_ai_during_render {
            return ScheduleVerdict::deny(BackoffReason::ConcurrencyLimit);
        }
        if self.running_ai_jobs >= self.policy.ai.max_parallel_requests {
            return ScheduleVerdict::deny(BackoffReason::ConcurrencyLimit);
        }
        if self.thermal == ThermalState::Critical {
            return ScheduleVerdict::deny(BackoffReason::Thermal);
        }
        ScheduleVerdict::ok()
    }

    pub fn commit_render_start(&mut self) {
        self.running_render_jobs += 1;
    }

    pub fn commit_render_end(&mut self) {
        self.running_render_jobs = self.running_render_jobs.saturating_sub(1);
    }

    pub fn commit_ai_start(&mut self) {
        self.running_ai_jobs += 1;
    }

    pub fn commit_ai_end(&mut self) {
        self.running_ai_jobs = self.running_ai_jobs.saturating_sub(1);
    }
}

impl ScheduleVerdict {
    pub fn ok() -> Self {
        Self {
            admitted: true,
            backoff_reason: None,
        }
    }

    pub fn deny(reason: BackoffReason) -> Self {
        Self {
            admitted: false,
            backoff_reason: Some(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::HardwareTier;

    #[test]
    fn admits_within_concurrency_then_denies() {
        let policy = GovernorPolicy::for_tier(HardwareTier::Medium);
        let mut sched = GovernorScheduler::new(policy);
        assert!(sched.admit_render().admitted);
        sched.commit_render_start();
        let denied = sched.admit_render();
        assert!(!denied.admitted);
        assert_eq!(denied.backoff_reason, Some(BackoffReason::ConcurrencyLimit));
        sched.commit_render_end();
        assert!(sched.admit_render().admitted);
    }

    #[test]
    fn memory_pressure_denies_render() {
        let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
        let mut sched = GovernorScheduler::new(policy);
        sched.set_available_ram_mb(100);
        let v = sched.admit_render();
        assert!(!v.admitted);
        assert_eq!(v.backoff_reason, Some(BackoffReason::MemoryPressure));
    }

    #[test]
    fn thermal_critical_denies_everything() {
        let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
        let mut sched = GovernorScheduler::new(policy);
        sched.set_thermal_state(ThermalState::Critical);
        assert!(!sched.admit_render().admitted);
        assert!(!sched.admit_ai().admitted);
    }

    #[test]
    fn warm_thermal_halves_render_concurrency() {
        let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
        let mut sched = GovernorScheduler::new(policy);
        sched.set_thermal_state(ThermalState::Warm);
        sched.commit_render_start();
        sched.commit_render_start();
        let v = sched.admit_render();
        assert!(!v.admitted);
    }

    #[test]
    fn ai_denied_during_render_on_low_tier() {
        let policy = GovernorPolicy::for_tier(HardwareTier::Low);
        let mut sched = GovernorScheduler::new(policy);
        sched.commit_render_start();
        let v = sched.admit_ai();
        assert!(!v.admitted);
    }

    #[test]
    fn critical_memory_pressure_denies_render() {
        // Phase 12 Task 27: scheduler must respect the memory monitor's
        // verdict even when there's plenty of RAM available in the
        // raw counter — the monitor saw the *process* RSS hit critical.
        let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
        let mut sched = GovernorScheduler::new(policy);
        sched.set_memory_state(crate::memory::MemoryState::Critical);
        let v = sched.admit_render();
        assert!(!v.admitted);
        assert_eq!(v.backoff_reason, Some(BackoffReason::MemoryPressure));
    }

    #[test]
    fn pressured_memory_halves_render_concurrency_cap() {
        // Phase 12 Task 27: at MemoryState::Pressured the scheduler
        // halves the thermal cap. Pro tier has max_concurrent_jobs = 3
        // → pressured cap = max(1, 3/2) = 1 → one committed render
        // fills the cap.
        let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
        assert!(
            policy.render.max_concurrent_jobs >= 2,
            "test depends on Pro tier having room to halve its cap"
        );
        let mut sched = GovernorScheduler::new(policy);
        // Baseline (no pressure) — full cap available.
        assert!(sched.admit_render().admitted);
        sched.commit_render_start();
        assert!(
            sched.admit_render().admitted,
            "without pressure a second render must fit in the Pro cap"
        );
        sched.commit_render_end();
        // Now apply pressure. With cap halved to 1, the running job
        // (just committed once) saturates the cap and the next
        // admission must be denied.
        sched.set_memory_state(crate::memory::MemoryState::Pressured);
        sched.commit_render_start();
        let v = sched.admit_render();
        assert!(!v.admitted);
        assert_eq!(v.backoff_reason, Some(BackoffReason::ConcurrencyLimit));
    }
}
