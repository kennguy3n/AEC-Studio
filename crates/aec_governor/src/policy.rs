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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GovernorPolicy {
    pub tier: HardwareTier,
    pub render: RenderPolicy,
    pub ai: AiPolicy,
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
