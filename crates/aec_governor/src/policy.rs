//! Per-tier policy defaults.

use serde::{Deserialize, Serialize};

use crate::tier::HardwareTier;

/// Canonical id for one of the bundled render presets. Mirrors the TS
/// `RenderPresetKey` union and the Rust `RenderPreset::id` field on the
/// `aec_render` side. Lives on the governor crate so policy structs can
/// reference presets without taking a runtime dependency on
/// `aec_render`, and so the type derives `Copy`.
///
/// The realtime-preview variant retains its `"eevee_preview"` on-wire id
/// for project-file compatibility — Phase 9 swapped the underlying
/// implementation (EEVEE-via-Blender → native PBR rasterizer) but kept
/// the string id stable so existing `.aecstudio` packages still load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetKey {
    /// Native PBR rasterizer realtime preview (replaces the pre-Phase-9
    /// EEVEE-via-Blender preview). On-wire id stays `"eevee_preview"`
    /// for backward compatibility — `rename` covers both directions of
    /// (de)serialization, so no `alias` is needed.
    #[serde(rename = "eevee_preview")]
    RealtimePreview,
    Quick,
    Standard,
    High,
    Studio,
    Walkthrough,
    Panorama,
}

impl PresetKey {
    /// The canonical short id used as the on-wire JSON value. Matches
    /// the strings used by the TS `RenderPresetKey` union and the
    /// `RenderPreset::id` field in `aec_render::preset`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RealtimePreview => "eevee_preview",
            Self::Quick => "quick",
            Self::Standard => "standard",
            Self::High => "high",
            Self::Studio => "studio",
            Self::Walkthrough => "walkthrough",
            Self::Panorama => "panorama",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RenderPolicy {
    pub default_samples: u32,
    pub default_tile_size_px: u32,
    pub max_concurrent_jobs: u32,
    pub max_concurrent_tiles: u32,
    /// Resolution multiplier the realtime PBR-rasterizer preview applies
    /// to its offscreen render target before upscaling to the viewport.
    /// Lower values trade fidelity for latency on tighter hardware.
    ///
    /// Serializes as `eevee_resolution_scale` and accepts that key on
    /// deserialize so existing `.aecstudio` projects continue to load.
    /// `rename` covers both directions, so no separate `alias` is needed.
    #[serde(rename = "eevee_resolution_scale")]
    pub preview_resolution_scale: f32,
    pub viewport_framebuffer_scale: f32,
    pub allow_background_ai_during_render: bool,
    /// Bundled preset the UI should preselect for this tier. Mirrors
    /// `aec_render::recommend_preset` (same short-id used by the TS
    /// `RenderPresetKey` union) but lives on the policy so the UI layer
    /// can show the recommendation without taking a runtime dependency
    /// on `aec_render`.
    pub recommended_preset_id: PresetKey,
}

impl RenderPolicy {
    /// Bundled preset id matching the tier — exposed so callers that
    /// already hold a `RenderPolicy` (e.g. the bridge IPC layer) don't
    /// have to reach back into `aec_render` to discover the
    /// recommendation. Returns the canonical short id string (e.g.
    /// `"quick"`, `"standard"`).
    pub fn recommended_preset_id(&self) -> &'static str {
        self.recommended_preset_id.as_str()
    }
}

/// AI model size tier. Maps 1:1 to a Ternary-Bonsai 1.58-bit GGUF Q2_0
/// file on disk; see [`aec_ai::ModelTier`] for the on-disk filenames and
/// the canonical BLAKE3 checksums.
///
/// | Tier   | Family               | Quant      | On disk  | Resident |
/// |--------|----------------------|------------|----------|----------|
/// | Small  | Ternary-Bonsai 1.7B  | 1.58-bit   |   442 MiB |  ~1.5 GiB |
/// | Medium | Ternary-Bonsai 4B    | 1.58-bit   |  1.00 GiB |  ~2.5 GiB |
/// | Large  | Ternary-Bonsai 8B    | 1.58-bit   |  2.03 GiB |  ~4.5 GiB |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiModelTier {
    /// Ternary-Bonsai 1.7B (1.58-bit GGUF Q2_0). Default for the Low tier.
    Small,
    /// Ternary-Bonsai 4B (1.58-bit GGUF Q2_0). Default for the High tier.
    Medium,
    /// Ternary-Bonsai 8B (1.58-bit GGUF Q2_0). Default for the Pro tier.
    Large,
}

impl AiModelTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AiPolicy {
    pub model_tier: AiModelTier,
    pub max_parallel_requests: u32,
    pub max_context_tokens: u32,
}

/// Phase 18 Group C Task 17 — per-tier image-gen governor policy.
///
/// Image-gen is single-model (no tier slug); the per-tier knobs we
/// vary are the lifecycle timings and concurrency limits. The
/// sidecar's resident-memory footprint (5–10× a text GGUF), GPU /
/// CPU contention with the path-tracer, and cold-spawn latency make
/// these the governance knobs that actually matter.
///
/// The bridge reads these into [`crate::ai::image_gen::ImageGenRuntimeConfig`]
/// at boot so a Low-tier laptop and a Pro-tier workstation get
/// runtime configs that match their hardware budget.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImageGenPolicy {
    /// How long the runtime may sit in `Ready` with no requests
    /// before the bridge unloads the sidecar. Low-tier hardware
    /// evicts much more aggressively (memory pressure) than
    /// Pro-tier (no pressure → keep ready for snappy successive
    /// generates).
    pub idle_timeout_secs: u32,
    /// Cold-spawn / health-load budget. Larger SD models (SDXL, FLUX)
    /// can take 30+ seconds to mmap, so we generously over-provision
    /// on every tier; on Low we cap shorter because we'd rather fail
    /// fast and tell the user "your laptop can't run this model".
    pub load_budget_secs: u32,
    /// Maximum concurrent in-flight `generate` requests. Each
    /// request is sequential on the sidecar, but a queue depth >1
    /// lets the renderer batch a small UI burst (e.g. quick
    /// re-prompt). Low tier caps at 1; Pro at 3.
    pub max_parallel_requests: u32,
    /// Whether the bridge will spawn / reuse the image-gen sidecar
    /// while at least one path-traced render job is `Running` in the
    /// render queue. On Low / Medium the answer is `false` — a
    /// concurrent path-tracer would thrash CPU/GPU and crash the
    /// sidecar on cold spawn; on High / Pro the answer is `true`
    /// (the workstation can absorb both). This mirrors
    /// [`RenderPolicy::allow_background_ai_during_render`] for the
    /// text sidecar.
    pub allow_during_pathtraced_render: bool,
}

impl Default for ImageGenPolicy {
    /// Returns the Medium-tier policy. We mirror Medium because
    /// that's the same "safe middle ground" the bridge advertises
    /// as the boot default (see [`crate::bridge::image_gen_active_policy`]
    /// in `aec_bridge`). The intent of this impl is to back
    /// `#[serde(default)]` on the [`GovernorPolicy::image_gen`]
    /// field — if a future binary deserializes a [`GovernorPolicy`]
    /// JSON written by an older version that pre-dates the
    /// `image_gen` field, the field comes in as Medium-tier
    /// defaults rather than failing the entire decode with a
    /// missing-field error. Construction at runtime always goes
    /// through [`GovernorPolicy::for_tier`] which picks the
    /// tier-correct values explicitly, so this `Default` is only
    /// ever observed on the deserialize-from-stale-data path.
    fn default() -> Self {
        Self {
            idle_timeout_secs: 120,
            load_budget_secs: 60,
            max_parallel_requests: 1,
            allow_during_pathtraced_render: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GovernorPolicy {
    pub tier: HardwareTier,
    pub render: RenderPolicy,
    pub ai: AiPolicy,
    /// Phase 18 Group C — image-gen sidecar policy. See
    /// [`ImageGenPolicy`] for the per-tier semantics.
    ///
    /// `#[serde(default)]` so a [`GovernorPolicy`] JSON written by
    /// a pre-Group-C binary (no `image_gen` field) still
    /// deserializes — the field comes in as
    /// [`ImageGenPolicy::default`] (Medium-tier) instead of failing
    /// the whole decode. Defensive: no caller persists
    /// [`GovernorPolicy`] today, but the type implements `Deserialize`
    /// so future config-file persistence MUST round-trip across
    /// schema additions without bricking older configs.
    #[serde(default)]
    pub image_gen: ImageGenPolicy,
    pub mesh_cache_budget_mb: u32,
}

impl GovernorPolicy {
    pub fn for_tier(tier: HardwareTier) -> Self {
        match tier {
            HardwareTier::Low => Self {
                tier,
                render: RenderPolicy {
                    default_samples: 32,
                    default_tile_size_px: 128,
                    max_concurrent_jobs: 1,
                    max_concurrent_tiles: 1,
                    preview_resolution_scale: 0.5,
                    viewport_framebuffer_scale: 0.75,
                    allow_background_ai_during_render: false,
                    recommended_preset_id: PresetKey::Quick,
                },
                ai: AiPolicy {
                    model_tier: AiModelTier::Small,
                    max_parallel_requests: 1,
                    max_context_tokens: 4096,
                },
                image_gen: ImageGenPolicy {
                    idle_timeout_secs: 60,
                    load_budget_secs: 45,
                    max_parallel_requests: 1,
                    allow_during_pathtraced_render: false,
                },
                mesh_cache_budget_mb: 512,
            },
            HardwareTier::Medium => Self {
                tier,
                render: RenderPolicy {
                    default_samples: 64,
                    default_tile_size_px: 192,
                    max_concurrent_jobs: 1,
                    max_concurrent_tiles: 2,
                    preview_resolution_scale: 0.75,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: false,
                    recommended_preset_id: PresetKey::Standard,
                },
                ai: AiPolicy {
                    model_tier: AiModelTier::Small,
                    max_parallel_requests: 2,
                    max_context_tokens: 6144,
                },
                image_gen: ImageGenPolicy {
                    idle_timeout_secs: 120,
                    load_budget_secs: 60,
                    max_parallel_requests: 1,
                    allow_during_pathtraced_render: false,
                },
                mesh_cache_budget_mb: 1024,
            },
            HardwareTier::High => Self {
                tier,
                render: RenderPolicy {
                    default_samples: 128,
                    default_tile_size_px: 256,
                    max_concurrent_jobs: 2,
                    max_concurrent_tiles: 4,
                    preview_resolution_scale: 1.0,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: true,
                    recommended_preset_id: PresetKey::High,
                },
                ai: AiPolicy {
                    model_tier: AiModelTier::Medium,
                    max_parallel_requests: 2,
                    max_context_tokens: 8192,
                },
                image_gen: ImageGenPolicy {
                    idle_timeout_secs: 180,
                    load_budget_secs: 75,
                    max_parallel_requests: 2,
                    allow_during_pathtraced_render: true,
                },
                mesh_cache_budget_mb: 2048,
            },
            HardwareTier::Pro => Self {
                tier,
                render: RenderPolicy {
                    default_samples: 256,
                    default_tile_size_px: 256,
                    max_concurrent_jobs: 3,
                    max_concurrent_tiles: 6,
                    preview_resolution_scale: 1.0,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: true,
                    recommended_preset_id: PresetKey::Studio,
                },
                ai: AiPolicy {
                    model_tier: AiModelTier::Large,
                    max_parallel_requests: 3,
                    max_context_tokens: 16384,
                },
                image_gen: ImageGenPolicy {
                    idle_timeout_secs: 300,
                    load_budget_secs: 90,
                    max_parallel_requests: 3,
                    allow_during_pathtraced_render: true,
                },
                mesh_cache_budget_mb: 4096,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_tier_yields_more_samples() {
        let low = GovernorPolicy::for_tier(HardwareTier::Low);
        let pro = GovernorPolicy::for_tier(HardwareTier::Pro);
        assert!(low.render.default_samples < pro.render.default_samples);
        assert!(low.render.max_concurrent_jobs < pro.render.max_concurrent_jobs);
        assert!(low.ai.max_context_tokens < pro.ai.max_context_tokens);
    }

    #[test]
    fn low_tier_disallows_background_ai() {
        let low = GovernorPolicy::for_tier(HardwareTier::Low);
        assert!(!low.render.allow_background_ai_during_render);
    }

    #[test]
    fn image_gen_policy_scales_monotonically_with_tier() {
        // Phase 18 Group C Task 17 — higher hardware tier ⇒ longer
        // resident window, larger spawn budget, more in-flight
        // requests. Pinning this prevents future tier reshuffles
        // from accidentally regressing image-gen capacity.
        let low = GovernorPolicy::for_tier(HardwareTier::Low).image_gen;
        let medium = GovernorPolicy::for_tier(HardwareTier::Medium).image_gen;
        let high = GovernorPolicy::for_tier(HardwareTier::High).image_gen;
        let pro = GovernorPolicy::for_tier(HardwareTier::Pro).image_gen;
        assert!(low.idle_timeout_secs < medium.idle_timeout_secs);
        assert!(medium.idle_timeout_secs < high.idle_timeout_secs);
        assert!(high.idle_timeout_secs < pro.idle_timeout_secs);
        assert!(low.load_budget_secs < medium.load_budget_secs);
        assert!(medium.load_budget_secs < high.load_budget_secs);
        assert!(high.load_budget_secs < pro.load_budget_secs);
        assert!(low.max_parallel_requests <= medium.max_parallel_requests);
        assert!(medium.max_parallel_requests < high.max_parallel_requests);
        assert!(high.max_parallel_requests < pro.max_parallel_requests);
    }

    #[test]
    fn low_and_medium_tiers_pause_image_gen_during_pathtraced_render() {
        // Phase 18 Group C Task 17 — concurrent diffusion + path-tracer
        // on a Low / Medium machine thrashes shared CPU/GPU resources
        // and can OOM the sidecar at spawn. The bridge enforces this
        // by inspecting the render queue for `Running` non-realtime
        // jobs before spawning the image-gen sidecar. Pinned here so
        // changing the policy requires updating the test (and thus
        // surfacing the change in review).
        assert!(
            !GovernorPolicy::for_tier(HardwareTier::Low)
                .image_gen
                .allow_during_pathtraced_render
        );
        assert!(
            !GovernorPolicy::for_tier(HardwareTier::Medium)
                .image_gen
                .allow_during_pathtraced_render
        );
        assert!(
            GovernorPolicy::for_tier(HardwareTier::High)
                .image_gen
                .allow_during_pathtraced_render
        );
        assert!(
            GovernorPolicy::for_tier(HardwareTier::Pro)
                .image_gen
                .allow_during_pathtraced_render
        );
    }

    #[test]
    fn image_gen_policy_default_matches_medium_tier_for_serde_default_fallback() {
        // `#[serde(default)]` on `GovernorPolicy::image_gen` relies on
        // `ImageGenPolicy::default` returning the Medium-tier policy,
        // because Medium is the "safe middle ground" the bridge
        // advertises as the boot default. Pinning the equality here
        // keeps the default impl from drifting away from the for_tier
        // values without a corresponding test failure.
        let default_policy = ImageGenPolicy::default();
        let medium_policy = GovernorPolicy::for_tier(HardwareTier::Medium).image_gen;
        assert_eq!(default_policy, medium_policy);
    }

    #[test]
    fn governor_policy_deserializes_legacy_json_without_image_gen_field() {
        // Forward-compat: a `GovernorPolicy` JSON written by a
        // pre-Group-C binary will have no `image_gen` field. The
        // `#[serde(default)]` attribute on `GovernorPolicy::image_gen`
        // must cause that missing field to come in as
        // `ImageGenPolicy::default` (Medium-tier) rather than fail
        // the whole decode with a missing-field error. Without this
        // attribute, any future config-file persistence path would
        // brick the moment a Group-C-aware binary read a Group-A-era
        // config.
        // We construct the legacy JSON by serializing a full
        // Medium-tier policy and then *removing* the `image_gen`
        // field, rather than hand-writing the JSON. This keeps the
        // test resilient to future rename / alias attributes on
        // unrelated fields (e.g. `eevee_resolution_scale` on
        // `RenderPolicy`) — only the `image_gen` removal is the
        // property under test.
        let medium_full = GovernorPolicy::for_tier(HardwareTier::Medium);
        let mut as_value =
            serde_json::to_value(medium_full).expect("Medium policy must round-trip to JSON value");
        let obj = as_value
            .as_object_mut()
            .expect("GovernorPolicy must serialize as a JSON object");
        assert!(
            obj.remove("image_gen").is_some(),
            "expected `image_gen` field on serialized policy to remove"
        );
        let legacy_json =
            serde_json::to_string(&as_value).expect("legacy JSON must reserialize cleanly");
        let decoded: GovernorPolicy = serde_json::from_str(&legacy_json)
            .expect("legacy GovernorPolicy without image_gen must still decode");
        assert_eq!(decoded.image_gen, ImageGenPolicy::default());
        assert_eq!(decoded.tier, HardwareTier::Medium);
    }

    #[test]
    fn each_tier_advertises_its_recommended_preset() {
        // Pinned by ARCHITECTURE.md §10.2 — same mapping as
        // `aec_render::recommend_preset`. Cross-tested against the
        // render crate's `RenderPreset::from_quality` so the strings
        // and the enum stay in lockstep.
        let cases = [
            (HardwareTier::Low, "quick"),
            (HardwareTier::Medium, "standard"),
            (HardwareTier::High, "high"),
            (HardwareTier::Pro, "studio"),
        ];
        for (tier, expected) in cases {
            let policy = GovernorPolicy::for_tier(tier);
            assert_eq!(
                policy.render.recommended_preset_id(),
                expected,
                "tier {tier:?} should recommend `{expected}`"
            );
        }
    }
}
