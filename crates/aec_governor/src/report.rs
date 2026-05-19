//! Human-readable profile report (renderable by the status bar / Home page).

use serde::{Deserialize, Serialize};

use crate::profiler::HardwareProfile;
use crate::tier::HardwareTier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileReport {
    pub tier: HardwareTier,
    pub headline: String,
    pub bullets: Vec<String>,
}

impl ProfileReport {
    pub fn build(profile: &HardwareProfile) -> Self {
        let tier = HardwareTier::classify(profile);
        let headline = format!(
            "{} · {} cores · {} GB RAM · {} ({} GB VRAM)",
            tier.as_str().to_uppercase(),
            profile.cpu.logical_cores,
            profile.total_ram_mb / 1024,
            profile.gpu.model,
            profile.gpu.vram_mb / 1024,
        );
        let mut bullets = Vec::new();
        bullets.push(format!("CPU: {}", profile.cpu.model));
        if !profile.cpu.features.is_empty() {
            bullets.push(format!("Features: {}", profile.cpu.features.join(", ")));
        }
        bullets.push(format!(
            "GPU: {} ({})",
            profile.gpu.model, profile.gpu.backend
        ));
        if !profile.gpu.accelerators.is_empty() {
            bullets.push(format!(
                "Accelerators: {}",
                profile.gpu.accelerators.join(", ")
            ));
        }
        bullets.push(format!("OS: {}", profile.os));
        Self {
            tier,
            headline,
            bullets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiler::{CpuProfile, GpuProfile};

    #[test]
    fn report_mentions_tier_and_gpu() {
        let p = HardwareProfile {
            cpu: CpuProfile {
                model: "Apple M3 Max".into(),
                physical_cores: 12,
                logical_cores: 16,
                features: vec!["neon".into()],
            },
            total_ram_mb: 32 * 1024,
            available_ram_mb: 22 * 1024,
            os: "macOS 14".into(),
            gpu: GpuProfile {
                vendor: "Apple".into(),
                model: "M3 Max GPU".into(),
                vram_mb: 24 * 1024,
                backend: "metal".into(),
                accelerators: vec!["neural_engine".into()],
            },
        };
        let report = ProfileReport::build(&p);
        assert_eq!(report.tier, HardwareTier::Pro);
        assert!(report.headline.contains("PRO"));
        assert!(report.headline.contains("M3 Max"));
    }
}
