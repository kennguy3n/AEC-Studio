//! wgpu renderer scaffold. Owns the device, queue, and the per-frame
//! command encoder.
//!
//! Two construction paths are offered:
//!
//! - [`ViewportRenderer::new_headless`] — full adapter + device
//!   acquisition with `force_fallback_adapter = true`. Used by the
//!   bridge unit-tests and headless CI runs. Falls back through
//!   `HighPerformance → LowPower → fallback` so machines without a
//!   real GPU still produce a working device.
//! - [`ViewportRenderer::new_headless_minimal`] — no adapter / no
//!   device. Used by pure-math tests that just need the shader
//!   strings, the backend tag, or a struct instance to thread
//!   through the type system without paying the adapter cost.
//!
//! The renderer is deliberately *thin* on top of wgpu: scene → vertex
//! buffers translation lives in [`crate::scene`], material binding
//! lives in [`crate::material_preview`], and the shaders are checked
//! in under `shaders/`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("could not request a wgpu adapter")]
    NoAdapter,
    #[error("wgpu device request failed: {0}")]
    DeviceRequest(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererBackend {
    Vulkan,
    Metal,
    Dx12,
    Gl,
    Fallback,
}

impl RendererBackend {
    pub fn as_wgpu(self) -> wgpu::Backends {
        match self {
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Metal => wgpu::Backends::METAL,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Gl => wgpu::Backends::GL,
            Self::Fallback => wgpu::Backends::all(),
        }
    }
}

/// Owns the wgpu device + queue. Constructed once at app startup.
pub struct ViewportRenderer {
    pub backend: RendererBackend,
    pub instance: wgpu::Instance,
    pub device: Option<wgpu::Device>,
    pub queue: Option<wgpu::Queue>,
    /// Adapter info, if one was successfully requested.
    pub adapter_info: Option<wgpu::AdapterInfo>,
}

impl ViewportRenderer {
    /// Construct a renderer *without* requesting an adapter. Useful for
    /// pure-unit tests on platforms without a GPU.
    pub fn new_headless_minimal(backend: RendererBackend) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: backend.as_wgpu(),
            ..Default::default()
        });
        Self {
            backend,
            instance,
            device: None,
            queue: None,
            adapter_info: None,
        }
    }

    /// Acquire a real adapter + device. Walks the
    /// `HighPerformance → LowPower → force_fallback` ladder so that
    /// the renderer still constructs on machines without a discrete
    /// GPU (and on CI runners that only expose the llvmpipe fallback
    /// adapter).
    ///
    /// On success the device, queue, and adapter info are stored on
    /// `self`. On failure returns [`RendererError::NoAdapter`].
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::NoAdapter`] when none of the three
    /// adapter-request rungs succeed, or
    /// [`RendererError::DeviceRequest`] when the adapter succeeds but
    /// the device request fails (typically OOM at adapter level).
    pub fn new_headless(backend: RendererBackend) -> Result<Self, RendererError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: backend.as_wgpu(),
            ..Default::default()
        });
        // Try the three rungs in order. `force_fallback_adapter` is
        // only set on the third attempt — wgpu otherwise prefers a
        // real adapter when one is available.
        let adapter = pollster::block_on(async {
            for (pref, force) in [
                (wgpu::PowerPreference::HighPerformance, false),
                (wgpu::PowerPreference::LowPower, false),
                (wgpu::PowerPreference::LowPower, true),
            ] {
                if let Some(a) = instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: pref,
                        compatible_surface: None,
                        force_fallback_adapter: force,
                    })
                    .await
                {
                    return Some(a);
                }
            }
            None
        })
        .ok_or(RendererError::NoAdapter)?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(async {
            adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aec-viewport-device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_defaults(),
                    },
                    None,
                )
                .await
        })
        .map_err(|e| RendererError::DeviceRequest(e.to_string()))?;
        Ok(Self {
            backend,
            instance,
            device: Some(device),
            queue: Some(queue),
            adapter_info: Some(adapter_info),
        })
    }

    /// Snapshot of GPU info suitable for the governor's hardware
    /// profiler. Returns `None` when no adapter has been acquired.
    pub fn gpu_descriptor(&self) -> Option<GpuDescriptor> {
        self.adapter_info.as_ref().map(|info| GpuDescriptor {
            vendor: vendor_name(info.vendor),
            model: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
        })
    }

    /// Returns the shader source strings the renderer ships with.
    pub fn shader_sources() -> ShaderSources {
        ShaderSources {
            geometry: include_str!("shaders/geometry.wgsl"),
            grid: include_str!("shaders/grid.wgsl"),
            selection: include_str!("shaders/selection.wgsl"),
            gizmo: include_str!("shaders/gizmo.wgsl"),
        }
    }
}

/// GPU info surface for the governor. Independent of `wgpu`'s
/// `AdapterInfo` so downstream code doesn't need to depend on wgpu.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GpuDescriptor {
    pub vendor: String,
    pub model: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
}

fn vendor_name(id: u32) -> String {
    match id {
        0x10DE => "NVIDIA".into(),
        0x1002 => "AMD".into(),
        0x8086 => "Intel".into(),
        0x106B => "Apple".into(),
        0x13B5 => "ARM".into(),
        0x5143 => "Qualcomm".into(),
        0x10005 => "Mesa".into(),
        0 => "Software".into(),
        other => format!("0x{other:04X}"),
    }
}

pub struct ShaderSources {
    pub geometry: &'static str,
    pub grid: &'static str,
    pub selection: &'static str,
    pub gizmo: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_sources_are_non_empty() {
        let s = ViewportRenderer::shader_sources();
        assert!(!s.geometry.is_empty());
        assert!(!s.grid.is_empty());
        assert!(!s.selection.is_empty());
        assert!(!s.gizmo.is_empty());
    }

    #[test]
    fn headless_minimal_constructs() {
        let r = ViewportRenderer::new_headless_minimal(RendererBackend::Fallback);
        assert!(r.device.is_none());
        assert!(r.queue.is_none());
        assert!(r.gpu_descriptor().is_none());
    }

    #[test]
    fn new_headless_requests_device_when_adapter_available() {
        // CI runners may not have any adapter at all; that's fine,
        // the call returns NoAdapter and we skip the assertion.
        match ViewportRenderer::new_headless(RendererBackend::Fallback) {
            Ok(r) => {
                assert!(r.device.is_some());
                assert!(r.queue.is_some());
                let info = r.gpu_descriptor().expect("descriptor present");
                assert!(!info.vendor.is_empty());
                assert!(!info.backend.is_empty());
            }
            Err(RendererError::NoAdapter) => {
                // No adapter at all on this runner — accept.
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn vendor_name_maps_common_ids() {
        assert_eq!(vendor_name(0x10DE), "NVIDIA");
        assert_eq!(vendor_name(0x1002), "AMD");
        assert_eq!(vendor_name(0x8086), "Intel");
        assert_eq!(vendor_name(0), "Software");
        assert!(vendor_name(0x9999).contains("0x"));
    }
}
