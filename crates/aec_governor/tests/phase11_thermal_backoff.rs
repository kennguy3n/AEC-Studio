//! Integration tests for Phase 11 Task 27 — resource governor thermal backoff.
//!
//! These tests exercise the full chain:
//!     `ManualSensor` → `ThermalMonitor` → `GovernorScheduler`
//! and verify that:
//!
//! 1. A reading above the `warm` threshold halves render-job concurrency
//!    (AI admission is unchanged in `Warm` — see
//!    `GovernorScheduler::admit_ai` in `src/scheduler.rs`, which only
//!    denies AI on `ThermalState::Critical`).
//! 2. A reading above the `critical` threshold denies *all* render and
//!    AI admissions with `BackoffReason::Thermal`.
//! 3. Returning to a nominal reading restores full concurrency.
//!
//! These complement the parser-level unit tests in
//! `src/thermal.rs::tests` — those pin the Linux sysfs / macOS pmset /
//! Windows WMI byte-level parsers; these pin the governor's externally-
//! observable contract.

use aec_governor::policy::GovernorPolicy;
use aec_governor::scheduler::{BackoffReason, GovernorScheduler};
use aec_governor::thermal::{ManualSensor, ThermalMonitor, ThermalThresholds};
use aec_governor::tier::HardwareTier;
use aec_governor::ui_report::ThermalState;

fn fresh_pair() -> (ManualSensor, ThermalMonitor, GovernorScheduler) {
    let manual = ManualSensor::new();
    let mon = ThermalMonitor::new(Box::new(manual.clone()), ThermalThresholds::default());
    // Pro-tier policy gives us a wide concurrency window so we can
    // observe the thermal-driven cap clearly.
    let sched = GovernorScheduler::new(GovernorPolicy::for_tier(HardwareTier::Pro));
    (manual, mon, sched)
}

#[test]
fn warm_reading_halves_render_concurrency_and_blocks_one_admission() {
    let (manual, mut mon, mut sched) = fresh_pair();
    let max = sched.admit_render(); // pre-flight, should admit
    assert!(max.admitted);

    manual.set_celsius(85.0); // > warm_celsius (80) but < critical (95)
    let state = mon.apply_to_scheduler(&mut sched);
    assert_eq!(state, ThermalState::Warm);

    // Pro tier admits up to 3 render jobs concurrently
    // (see GovernorPolicy::for_tier(HardwareTier::Pro)). Warm clips
    // that to max(1, 3 - 1) = 2 via `scheduler::admit_render`'s
    // thermal branch, so we should be able to start exactly 2
    // before the 3rd is denied for ConcurrencyLimit.
    for _ in 0..2 {
        let v = sched.admit_render();
        assert!(v.admitted, "warm should still allow render starts");
        sched.commit_render_start();
    }
    let denied = sched.admit_render();
    assert!(!denied.admitted);
    assert_eq!(denied.backoff_reason, Some(BackoffReason::ConcurrencyLimit));
}

#[test]
fn critical_reading_denies_all_render_admissions_with_thermal_reason() {
    let (manual, mut mon, mut sched) = fresh_pair();

    manual.set_celsius(96.0); // > critical_celsius (95)
    let state = mon.apply_to_scheduler(&mut sched);
    assert_eq!(state, ThermalState::Critical);

    let v = sched.admit_render();
    assert!(!v.admitted);
    assert_eq!(v.backoff_reason, Some(BackoffReason::Thermal));
}

#[test]
fn critical_reading_pauses_ai_inference() {
    let (manual, mut mon, mut sched) = fresh_pair();

    manual.set_celsius(96.0);
    mon.apply_to_scheduler(&mut sched);

    let v = sched.admit_ai();
    assert!(!v.admitted);
    assert_eq!(v.backoff_reason, Some(BackoffReason::Thermal));
}

#[test]
fn returning_to_nominal_restores_full_concurrency() {
    let (manual, mut mon, mut sched) = fresh_pair();

    manual.set_celsius(96.0);
    mon.apply_to_scheduler(&mut sched);
    assert!(!sched.admit_render().admitted);

    manual.set_celsius(45.0);
    let state = mon.apply_to_scheduler(&mut sched);
    assert_eq!(state, ThermalState::Nominal);
    assert!(sched.admit_render().admitted);
}

#[test]
fn macos_speed_limit_axis_drives_critical_back_off_independent_of_temperature() {
    // Simulate macOS: no temperature reading available, but pmset
    // reports a 40 % speed-limit clip. Even without any
    // `max_cpu_celsius`, the governor must back off.
    let manual = ManualSensor::new();
    manual.set(Some(aec_governor::thermal::ThermalReading {
        max_cpu_celsius: None,
        cpu_speed_limit_ratio: Some(0.40),
        source: aec_governor::thermal::ThermalSource::Manual,
    }));
    let mut mon = ThermalMonitor::new(Box::new(manual.clone()), ThermalThresholds::default());
    let mut sched = GovernorScheduler::new(GovernorPolicy::for_tier(HardwareTier::Pro));

    let state = mon.apply_to_scheduler(&mut sched);
    assert_eq!(state, ThermalState::Critical);
    assert!(!sched.admit_render().admitted);
    assert!(!sched.admit_ai().admitted);
}
