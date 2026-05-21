//! Linux runtime soak — exercises the Linux-specific governor code
//! paths end-to-end.
//!
//! This complements the unit tests in `profiler.rs` and `tier.rs` by
//! validating that the integration shape of "profile a real Linux box
//! → classify a tier → derive a policy" holds together. We never call
//! external binaries — the test must pass on any Linux CI runner.

#![cfg(target_os = "linux")]

use aec_governor::policy::{AiModelTier, GovernorPolicy};
use aec_governor::profiler::{
    detect_linux_gpu, CpuProfile, GpuProfile, HardwareProfile, HardwareProfiler,
};
use aec_governor::{GovernorScheduler, HardwareTier};

fn synthetic_gpu(vendor: &str, vram_mb: u32) -> GpuProfile {
    GpuProfile {
        vendor: vendor.into(),
        model: format!("{vendor} synthetic"),
        vram_mb,
        backend: "vulkan".into(),
        accelerators: vec!["vulkan".into()],
    }
}

fn synthetic_cpu(cores: u32, brand: &str) -> CpuProfile {
    CpuProfile {
        model: brand.into(),
        physical_cores: cores.max(1) / 2,
        logical_cores: cores,
        features: vec!["avx2".into()],
    }
}

#[test]
fn detect_linux_gpu_never_panics_and_returns_filled_struct() {
    // The real probe reads /proc/driver/nvidia/version, runs lspci,
    // or falls back to vulkaninfo. None of those are guaranteed on
    // the CI runner — what matters is the call returns *something*.
    let probe = detect_linux_gpu();
    assert!(!probe.vendor.is_empty(), "vendor must be non-empty");
    assert!(!probe.model.is_empty(), "model must be non-empty");
    assert!(!probe.backend.is_empty(), "backend must be non-empty");
}

#[test]
fn profile_classify_and_policy_derive_for_low_end_linux_box() {
    let mut profiler = HardwareProfiler::new();
    let gpu = synthetic_gpu("Intel", 0);
    let profile = profiler.profile(gpu);
    let tier = HardwareTier::classify(&profile);

    // We can't assert a specific tier (CI runners vary) but Pro
    // requires VRAM, and we set VRAM to 0 — so the GPU-thirsty Pro
    // tier is unreachable.
    assert_ne!(
        tier,
        HardwareTier::Pro,
        "no-VRAM box must not classify as Pro"
    );

    let policy = GovernorPolicy::for_tier(tier);
    // Every tier yields a usable AI policy.
    let _ = policy.ai.model_tier;
}

#[test]
fn profile_classifies_high_end_linux_workstation() {
    // Synthesise the profile rather than the real machine so we can
    // assert a deterministic tier outcome.
    let profile = HardwareProfile {
        cpu: synthetic_cpu(32, "AMD Ryzen 9"),
        gpu: synthetic_gpu("NVIDIA", 24 * 1024),
        total_ram_mb: 64 * 1024,
        available_ram_mb: 48 * 1024,
        os: "Linux".into(),
    };
    let tier = HardwareTier::classify(&profile);
    assert_eq!(tier, HardwareTier::Pro);

    let policy = GovernorPolicy::for_tier(tier);
    // Pro tier should grant the headline AI model.
    assert!(matches!(
        policy.ai.model_tier,
        AiModelTier::Medium | AiModelTier::Large
    ));
    let _ = policy;
}

#[test]
fn scheduler_admits_and_rejects_jobs_with_linux_profile() {
    let profile = HardwareProfile {
        cpu: synthetic_cpu(8, "Linux CPU"),
        gpu: synthetic_gpu("AMD", 8 * 1024),
        total_ram_mb: 16 * 1024,
        available_ram_mb: 12 * 1024,
        os: "Linux".into(),
    };
    let tier = HardwareTier::classify(&profile);
    let policy = GovernorPolicy::for_tier(tier);
    let mut sched = GovernorScheduler::new(policy);
    sched.set_available_ram_mb(profile.available_ram_mb);

    // Render admission should succeed on a healthy box.
    let verdict = sched.admit_render();
    assert!(
        verdict.admitted,
        "first render admission must be allowed on a healthy box: {verdict:?}"
    );

    // AI admission likewise.
    let ai = sched.admit_ai();
    assert!(ai.admitted, "AI admission must be allowed: {ai:?}");

    // Once a user pauses the governor, every admission must be denied.
    sched.set_user_paused(true);
    assert!(!sched.admit_render().admitted);
    assert!(!sched.admit_ai().admitted);
}

// Phase 9 PR4 removed the Blender worker entirely; the previous
// `linux_blender_discovery_path_set_is_non_empty` smoke test no longer
// has a target binary to probe for. The native render pipeline carries
// its own coverage in `aec_render` (path tracer, scheduler, etc.) and
// does not need a Linux-specific discovery probe.
