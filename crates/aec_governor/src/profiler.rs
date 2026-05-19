//! Hardware profiler. Uses sysinfo for CPU + RAM + OS detection. GPU
//! detection is best-effort: on Linux/macOS/Windows the wgpu instance can be
//! queried for adapter info, but to keep this crate lightweight we accept an
//! externally supplied [`GpuProfile`] (the bridge fills it in from
//! `wgpu::Instance::enumerate_adapters`).

use serde::{Deserialize, Serialize};
use sysinfo::System;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuProfile {
    pub model: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
    /// Best-effort feature flags ("avx2", "avx512f", "neon", ...).
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuProfile {
    pub vendor: String,
    pub model: String,
    pub vram_mb: u32,
    /// "vulkan" | "metal" | "dx12" | "opengl" | "software".
    pub backend: String,
    pub accelerators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub cpu: CpuProfile,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub os: String,
    pub gpu: GpuProfile,
}

pub struct HardwareProfiler {
    system: System,
}

impl Default for HardwareProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HardwareProfiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareProfiler").finish_non_exhaustive()
    }
}

impl HardwareProfiler {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_memory();
        system.refresh_cpu_all();
        Self { system }
    }

    /// Build a `HardwareProfile` using the supplied GPU info (the bridge
    /// owns the wgpu instance and passes adapter info here).
    pub fn profile(&mut self, gpu: GpuProfile) -> HardwareProfile {
        self.system.refresh_memory();
        self.system.refresh_cpu_all();
        let physical = self.system.physical_core_count().unwrap_or(0) as u32;
        let logical = self.system.cpus().len() as u32;
        let model = self
            .system
            .cpus()
            .first()
            .map_or_else(|| "unknown".into(), |c| c.brand().to_string());
        let features = detect_cpu_features();
        let cpu = CpuProfile {
            model,
            physical_cores: physical,
            logical_cores: logical,
            features,
        };
        let total_ram_mb = self.system.total_memory() / (1024 * 1024);
        let available_ram_mb = self.system.available_memory() / (1024 * 1024);
        let os = format!(
            "{} {}",
            System::name().unwrap_or_else(|| "unknown".into()),
            System::os_version().unwrap_or_default(),
        )
        .trim()
        .to_string();
        HardwareProfile {
            cpu,
            total_ram_mb,
            available_ram_mb,
            os,
            gpu,
        }
    }
}

fn detect_cpu_features() -> Vec<String> {
    let mut features = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            features.push("avx2".into());
        }
        if is_x86_feature_detected!("avx512f") {
            features.push("avx512f".into());
        }
        if is_x86_feature_detected!("sse4.2") {
            features.push("sse4.2".into());
        }
        if is_x86_feature_detected!("fma") {
            features.push("fma".into());
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        features.push("neon".into());
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_capture_at_least_one_cpu_core() {
        let mut p = HardwareProfiler::new();
        let gpu = GpuProfile {
            vendor: "Unknown".into(),
            model: "Unknown".into(),
            vram_mb: 0,
            backend: "software".into(),
            accelerators: Vec::new(),
        };
        let prof = p.profile(gpu);
        assert!(prof.cpu.logical_cores >= 1);
        assert!(prof.total_ram_mb > 0);
    }
}
