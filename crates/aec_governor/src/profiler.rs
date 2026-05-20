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
        // AVX-VNNI and AVX-512 VNNI are critical for INT8 inference
        // throughput on Linux/Windows x86_64 — surface them so the
        // governor and AI runtime can pick the right kernel.
        if is_x86_feature_detected!("avx512vnni") {
            features.push("avx512vnni".into());
        }
        if is_x86_feature_detected!("avxvnni") {
            features.push("avxvnni".into());
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        features.push("neon".into());
    }
    features
}

/// Linux GPU detection probe results. The bridge passes wgpu-derived
/// info into [`HardwareProfiler::profile`], but on Linux we also want
/// a parallel best-effort sysprobe that picks up NVIDIA via
/// `/proc/driver/nvidia/version` and any other PCI display devices via
/// `lspci`. The function never fails — missing files or commands just
/// produce empty fields — so it is safe to call unconditionally.
#[cfg(target_os = "linux")]
pub fn detect_linux_gpu() -> GpuProfile {
    use std::process::Command;

    // NVIDIA proprietary driver leaves a marker file with driver version.
    let nvidia_marker = std::fs::read_to_string("/proc/driver/nvidia/version").ok();
    if let Some(content) = nvidia_marker {
        // First line typically: "NVRM version: NVIDIA UNIX x86_64 Kernel Module  535.86.10  ..."
        let first = content.lines().next().unwrap_or("").to_string();
        return GpuProfile {
            vendor: "NVIDIA".into(),
            model: first,
            vram_mb: 0,
            backend: "vulkan".into(),
            accelerators: vec!["cuda".into(), "vulkan".into()],
        };
    }

    // Fallback to lspci, which is present on every mainstream Linux
    // distro. We only need the first display-class line.
    if let Ok(output) = Command::new("lspci").arg("-mm").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.contains("VGA compatible controller") || line.contains("3D controller") {
                    let (vendor, model) = parse_lspci_line(line);
                    return GpuProfile {
                        vendor,
                        model,
                        vram_mb: 0,
                        backend: "vulkan".into(),
                        accelerators: vec!["vulkan".into()],
                    };
                }
            }
        }
    }

    // Final fallback: vulkaninfo summary line, then a software profile.
    if let Ok(output) = Command::new("vulkaninfo").arg("--summary").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().find(|l| l.contains("deviceName")) {
                let model = line.split('=').nth(1).unwrap_or("Vulkan device").trim();
                return GpuProfile {
                    vendor: "Unknown".into(),
                    model: model.to_string(),
                    vram_mb: 0,
                    backend: "vulkan".into(),
                    accelerators: vec!["vulkan".into()],
                };
            }
        }
    }

    GpuProfile {
        vendor: "Unknown".into(),
        model: "Unknown".into(),
        vram_mb: 0,
        backend: "software".into(),
        accelerators: Vec::new(),
    }
}

#[cfg(target_os = "linux")]
fn parse_lspci_line(line: &str) -> (String, String) {
    // lspci -mm format: "01:00.0 \"VGA compatible controller\" \"NVIDIA Corporation\" \"AD104\" ..."
    let parts: Vec<&str> = line.split('"').collect();
    let vendor = (*parts.get(3).unwrap_or(&"Unknown")).to_string();
    let model = (*parts.get(5).unwrap_or(&"Unknown")).to_string();
    (vendor, model)
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_gpu_probe_never_panics_and_returns_a_profile() {
        let gpu = detect_linux_gpu();
        // Regardless of whether the test host has a discrete GPU, the
        // probe must succeed and produce a profile with one of the
        // expected backend strings.
        let backends = ["vulkan", "software"];
        assert!(
            backends.contains(&gpu.backend.as_str()),
            "unexpected backend: {}",
            gpu.backend
        );
        assert!(!gpu.vendor.is_empty());
        assert!(!gpu.model.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_lspci_line_extracts_vendor_and_model() {
        let raw = r#"01:00.0 "VGA compatible controller" "NVIDIA Corporation" "AD104 [GeForce RTX 4070]" -r a1 "ASUSTeK" "DUAL""#;
        let (vendor, model) = parse_lspci_line(raw);
        assert_eq!(vendor, "NVIDIA Corporation");
        assert!(model.contains("AD104"));
    }
}
