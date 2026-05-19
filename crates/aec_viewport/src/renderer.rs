//! wgpu renderer scaffold. Owns the device, queue, and the per-frame
//! command encoder. Headless adapter creation is supported for tests
//! (see [`ViewportRenderer::new_headless`]).
//!
//! The renderer is deliberately *thin* on top of wgpu: scene → vertex
//! buffers translation lives in [`crate::scene`], material binding lives
//! in [`crate::material_preview`], and the shaders are checked in under
//! `shaders/`.

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
    }
}
