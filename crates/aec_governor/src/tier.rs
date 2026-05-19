//! Hardware tier classification.
//!
//! The tier drives all per-session policy defaults: render preset, sample
//! counts, AI model size, viewport framebuffer scale, EEVEE resolution, mesh
//! cache size, max simultaneous render jobs.

use serde::{Deserialize, Serialize};

use crate::profiler::HardwareProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareTier {
    Low,
    Medium,
    High,
    Pro,
}

impl HardwareTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Pro => "pro",
        }
    }

    /// Classify a [`HardwareProfile`] into a tier.
    ///
    /// Thresholds intentionally mirror the table in `ARCHITECTURE.md` so
    /// changes to the doc and the code can be cross-checked.
    pub fn classify(profile: &HardwareProfile) -> Self {
        let cores = profile.cpu.logical_cores;
        let ram_gb = profile.total_ram_mb / 1024;
        let vram_gb = profile.gpu.vram_mb / 1024;
        if cores >= 16 && ram_gb >= 32 && vram_gb >= 12 {
            Self::Pro
        } else if cores >= 8 && ram_gb >= 16 && vram_gb >= 8 {
            Self::High
        } else if cores >= 4 && ram_gb >= 8 && vram_gb >= 4 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiler::{CpuProfile, GpuProfile, HardwareProfile};

    fn profile(cores: u32, ram_mb: u64, vram_mb: u32) -> HardwareProfile {
        HardwareProfile {
            cpu: CpuProfile {
                model: "Test".into(),
                physical_cores: cores,
                logical_cores: cores,
                features: vec![],
            },
            total_ram_mb: ram_mb,
            available_ram_mb: ram_mb,
            os: "test".into(),
            gpu: GpuProfile {
                vendor: "Test".into(),
                model: "Test".into(),
                vram_mb,
                backend: "vulkan".into(),
                accelerators: vec![],
            },
        }
    }

    #[test]
    fn small_laptop_is_low() {
        let p = profile(4, 8 * 1024, 2 * 1024);
        assert_eq!(HardwareTier::classify(&p), HardwareTier::Low);
    }

    #[test]
    fn mid_laptop_is_medium() {
        let p = profile(4, 8 * 1024, 4 * 1024);
        assert_eq!(HardwareTier::classify(&p), HardwareTier::Medium);
    }

    #[test]
    fn enthusiast_is_high() {
        let p = profile(8, 16 * 1024, 8 * 1024);
        assert_eq!(HardwareTier::classify(&p), HardwareTier::High);
    }

    #[test]
    fn workstation_is_pro() {
        let p = profile(16, 32 * 1024, 12 * 1024);
        assert_eq!(HardwareTier::classify(&p), HardwareTier::Pro);
    }

    #[test]
    fn low_ram_capped_at_medium() {
        let p = profile(16, 4 * 1024, 12 * 1024);
        assert_eq!(HardwareTier::classify(&p), HardwareTier::Low);
    }
}
