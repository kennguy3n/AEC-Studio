//! Per-tier policy defaults.

use serde::{Deserialize, Serialize};

use crate::tier::HardwareTier;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RenderPolicy {
    pub default_samples: u32,
    pub default_tile_size_px: u32,
    pub max_concurrent_jobs: u32,
    pub max_concurrent_tiles: u32,
    pub eevee_resolution_scale: f32,
    pub viewport_framebuffer_scale: f32,
    pub allow_background_ai_during_render: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiModelTier {
    Small,
    Medium,
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
                    eevee_resolution_scale: 0.5,
                    viewport_framebuffer_scale: 0.75,
                    allow_background_ai_during_render: false,
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
                    eevee_resolution_scale: 0.75,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: false,
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
                    eevee_resolution_scale: 1.0,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: true,
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
                    eevee_resolution_scale: 1.0,
                    viewport_framebuffer_scale: 1.0,
                    allow_background_ai_during_render: true,
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
}
