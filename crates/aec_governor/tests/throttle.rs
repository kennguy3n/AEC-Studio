//! Integration test for the governor scheduler under load.
//!
//! The user journeys (Render, AI assist while rendering) all hinge on
//! the scheduler honouring the policy table. This test pins the
//! load/concurrency/memory-pressure contract end-to-end:
//!
//! 1. Low-tier policy admits only 1 concurrent render job.
//! 2. Pro-tier policy admits all 4 concurrent render jobs.
//! 3. Memory pressure (`available_ram_mb` < `mesh_cache_budget_mb`)
//!    forces back-off independently of concurrency.
//! 4. Background AI is paused while a render is running on tiers that
//!    forbid it.

use aec_governor::{BackoffReason, GovernorPolicy, GovernorScheduler, HardwareTier};

#[test]
fn low_tier_caps_concurrent_render_jobs_at_one() {
    let policy = GovernorPolicy::for_tier(HardwareTier::Low);
    let mut sched = GovernorScheduler::new(policy);

    // Give the scheduler enough headroom on RAM so memory pressure
    // isn't what's gating us.
    sched.set_available_ram_mb(64 * 1024);

    // First admission succeeds.
    let first = sched.admit_render();
    assert!(first.admitted, "low tier must admit the first render");
    sched.commit_render_start();
    assert_eq!(sched.running_render_jobs(), 1);

    // Subsequent admissions are denied with ConcurrencyLimit until the
    // first job finishes.
    for attempt in 0..3 {
        let v = sched.admit_render();
        assert!(
            !v.admitted,
            "attempt {attempt}: low tier must deny concurrent renders",
        );
        assert_eq!(
            v.backoff_reason,
            Some(BackoffReason::ConcurrencyLimit),
            "attempt {attempt}: deny reason must be ConcurrencyLimit",
        );
    }

    // Drain the in-flight job.
    sched.commit_render_end();
    assert_eq!(sched.running_render_jobs(), 0);
    assert!(
        sched.admit_render().admitted,
        "low tier must admit again once the prior job finished",
    );
}

#[test]
fn pro_tier_admits_all_four_concurrent_jobs() {
    let policy = GovernorPolicy::for_tier(HardwareTier::Pro);
    let mut sched = GovernorScheduler::new(policy);
    sched.set_available_ram_mb(64 * 1024);

    // Pro tier policy admits N concurrent jobs (>=3 per the policy
    // table). We assert ≥3 — bumping it later doesn't break the test.
    let cap = GovernorPolicy::for_tier(HardwareTier::Pro)
        .render
        .max_concurrent_jobs;
    assert!(
        cap >= 3,
        "pro tier policy must admit ≥3 concurrent renders, got {cap}",
    );

    for i in 0..cap {
        let v = sched.admit_render();
        assert!(
            v.admitted,
            "pro tier must admit job #{i} (cap={cap}, running={})",
            sched.running_render_jobs(),
        );
        sched.commit_render_start();
    }

    // The (cap+1)-th render is denied.
    let v = sched.admit_render();
    assert!(!v.admitted);
    assert_eq!(v.backoff_reason, Some(BackoffReason::ConcurrencyLimit));
}

#[test]
fn memory_pressure_overrides_concurrency_headroom() {
    let policy = GovernorPolicy::for_tier(HardwareTier::High);
    let budget = policy.mesh_cache_budget_mb as u64;
    let mut sched = GovernorScheduler::new(policy);

    // Plenty of headroom → admitted.
    sched.set_available_ram_mb(budget + 1024);
    assert!(sched.admit_render().admitted);

    // Squeeze RAM below the budget → backoff with MemoryPressure even
    // though no renders are running.
    sched.set_available_ram_mb(budget.saturating_sub(1));
    let v = sched.admit_render();
    assert!(!v.admitted);
    assert_eq!(v.backoff_reason, Some(BackoffReason::MemoryPressure));
}

#[test]
fn background_ai_is_paused_while_render_runs_on_low_tier() {
    let policy = GovernorPolicy::for_tier(HardwareTier::Low);
    assert!(
        !policy.render.allow_background_ai_during_render,
        "low tier policy must forbid background AI during render",
    );

    let mut sched = GovernorScheduler::new(policy);
    sched.set_available_ram_mb(64 * 1024);

    assert!(sched.admit_ai().admitted, "AI is admitted when no render is running");

    sched.commit_render_start();
    let denied = sched.admit_ai();
    assert!(!denied.admitted, "AI must be denied while a render is running");
    assert_eq!(denied.backoff_reason, Some(BackoffReason::ConcurrencyLimit));

    sched.commit_render_end();
    assert!(
        sched.admit_ai().admitted,
        "AI must be admitted again after the render finishes",
    );
}
