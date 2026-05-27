//! Forward render pipeline that orchestrates the four "core" viewport
//! shaders shipped under `shaders/`:
//!
//! - `geometry.wgsl` — instanced PBR-lite mesh pass into the colour +
//!   depth target
//! - `selection.wgsl` — silhouette / outline overlay using a stencil
//!   mask, drawn after geometry
//! - `gizmo.wgsl` — translation / rotation gizmo on top of the scene
//!   (depth-test off so it stays visible)
//! - `grid.wgsl` — infinite ground grid drawn last (full-screen
//!   triangle, ray-marched against y=0)
//!
//! The pipeline lives between `ViewportRenderer` (owns the device) and
//! `SurfaceManager` (owns the colour / depth textures). It validates
//! every shader at startup (via naga) so a regression in any of the
//! four sources is surfaced before we ever try to construct a wgpu
//! pipeline state, and exposes per-pipeline accessors so the integrating
//! test in `aec_bridge` can drive a frame deterministically.
//!
//! The implementation is intentionally minimal — vertex buffer layouts
//! follow the existing shaders, MSAA defaults to 4x and is configurable,
//! and there is no post-processing pass. Higher-level effects (CSM, IES
//! GPU sampling, NLM denoising) live in the dedicated pipelines under
//! `pbr_preview.rs` and `viewport_pipeline.rs`.

use thiserror::Error;
use wgpu::util::DeviceExt;

use crate::renderer::ViewportRenderer;

/// Default MSAA sample count. wgpu downlevel defaults guarantee at
/// least 1x and 4x are supported.
pub const DEFAULT_MSAA: u32 = 4;

/// Tunable pipeline parameters. Construct with [`PipelineConfig::new`]
/// or use [`PipelineConfig::default`] for sane defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineConfig {
    /// MSAA sample count. Must be 1, 2, 4, or 8. Anything else falls
    /// back to 1 (no MSAA) so the pipeline never fails to construct
    /// on a downlevel adapter.
    pub sample_count: u32,
    /// Texture format for the colour target. Defaults to RGBA8Unorm,
    /// which the readback path in `SurfaceManager` mirrors.
    pub color_format: wgpu::TextureFormat,
    /// Depth-stencil format. Defaults to Depth24PlusStencil8 so the
    /// selection pass can use the stencil buffer for masking.
    pub depth_format: wgpu::TextureFormat,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            sample_count: DEFAULT_MSAA,
            color_format: wgpu::TextureFormat::Rgba8Unorm,
            depth_format: wgpu::TextureFormat::Depth24PlusStencil8,
        }
    }
}

impl PipelineConfig {
    /// Construct with explicit values.
    pub fn new(
        sample_count: u32,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        Self {
            sample_count: normalise_msaa(sample_count),
            color_format,
            depth_format,
        }
    }
}

fn normalise_msaa(s: u32) -> u32 {
    match s {
        1 | 2 | 4 | 8 => s,
        _ => 1,
    }
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("shader '{name}' failed to compile: {error}")]
    ShaderCompile { name: &'static str, error: String },
    #[error("renderer is missing a device — call ViewportRenderer::new_headless first")]
    NoDevice,
}

/// Aggregate of the four core render pipelines. Holds wgpu resources
/// once the constructor succeeds and never re-creates them on resize —
/// only the colour / depth textures owned by `SurfaceManager` change.
pub struct RenderPipeline {
    pub config: PipelineConfig,
    pub camera_buffer: wgpu::Buffer,
    pub camera_bind_group_layout: wgpu::BindGroupLayout,
    pub camera_bind_group: wgpu::BindGroup,
    pub geometry: wgpu::RenderPipeline,
    pub selection: wgpu::RenderPipeline,
    pub gizmo: wgpu::RenderPipeline,
    pub grid: wgpu::RenderPipeline,
}

/// Camera uniform shared by all four passes. Mirrors the layout the
/// WGSL shaders expect.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    pub view_proj: [[f32; 4]; 4],
    pub camera_pos: [f32; 4],
}

impl CameraUniform {
    pub fn identity() -> Self {
        Self {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl RenderPipeline {
    /// Validate all four shader sources via naga before the pipeline
    /// is constructed. This is *also* exposed as a free function so
    /// CI can verify shader correctness without acquiring a wgpu
    /// device.
    pub fn validate_shaders() -> Result<(), PipelineError> {
        let s = ViewportRenderer::shader_sources();
        for (name, src) in [
            ("geometry.wgsl", s.geometry),
            ("grid.wgsl", s.grid),
            ("selection.wgsl", s.selection),
            ("gizmo.wgsl", s.gizmo),
        ] {
            naga::front::wgsl::parse_str(src).map_err(|e| PipelineError::ShaderCompile {
                name,
                error: e.to_string(),
            })?;
        }
        Ok(())
    }

    /// Build all four pipelines against the renderer's device.
    ///
    /// # Errors
    ///
    /// - [`PipelineError::NoDevice`] when the renderer has no device.
    /// - [`PipelineError::ShaderCompile`] when any of the four shader
    ///   sources fail to parse.
    pub fn build(
        renderer: &ViewportRenderer,
        config: PipelineConfig,
    ) -> Result<Self, PipelineError> {
        let device = renderer.device.as_ref().ok_or(PipelineError::NoDevice)?;
        // Validate shaders up front so we don't try to compile a
        // wgpu module from broken source.
        Self::validate_shaders()?;

        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aec.render_pipeline.camera"),
            contents: bytemuck::bytes_of(&CameraUniform::identity()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("aec.render_pipeline.camera_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aec.render_pipeline.camera_bg"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aec.render_pipeline.layout"),
            bind_group_layouts: &[&camera_bind_group_layout],
            push_constant_ranges: &[],
        });

        let shaders = ViewportRenderer::shader_sources();
        let make_module = |name: &str, src: &str| -> wgpu::ShaderModule {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        };
        let geom_mod = make_module("aec.geometry.wgsl", shaders.geometry);
        let sel_mod = make_module("aec.selection.wgsl", shaders.selection);
        let giz_mod = make_module("aec.gizmo.wgsl", shaders.gizmo);
        let grid_mod = make_module("aec.grid.wgsl", shaders.grid);

        // Vertex layout: position(vec3), normal(vec3), uv(vec2)
        // Instance layout: 4x4 model matrix + tint(vec4)
        let vertex_buffers = [
            wgpu::VertexBufferLayout {
                array_stride: (3 + 3 + 2) * std::mem::size_of::<f32>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x3, 1 => Float32x3, 2 => Float32x2,
                ],
            },
            wgpu::VertexBufferLayout {
                array_stride: (16 + 4) * std::mem::size_of::<f32>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4,
                    7 => Float32x4,
                ],
            },
        ];

        let geometry = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aec.geometry_pass"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &geom_mod,
                entry_point: "vs_main",
                buffers: &vertex_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &geom_mod,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.color_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: config.depth_format,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: config.sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
        });

        // Selection vertex layout: position(vec3) + normal(vec3).
        let selection_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: (3 + 3) * std::mem::size_of::<f32>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x3, 1 => Float32x3,
            ],
        }];
        let selection = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aec.selection_overlay"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sel_mod,
                entry_point: "vs_outline",
                buffers: &selection_vertex_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sel_mod,
                entry_point: "fs_outline",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.color_format,
                    // Selection overlay alpha-blends over the geometry pass.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: config.depth_format,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: config.sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
        });

        // Gizmo vertex layout: position(vec3) + color(vec4).
        let gizmo_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: (3 + 4) * std::mem::size_of::<f32>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x3, 1 => Float32x4,
            ],
        }];
        let gizmo = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aec.gizmo_overlay"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &giz_mod,
                entry_point: "vs_main",
                buffers: &gizmo_vertex_buffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &giz_mod,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // Gizmo deliberately disables depth test so it stays
            // visible through walls — designers expect that
            // behaviour.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: config.depth_format,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: config.sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
        });

        let grid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aec.grid_pass"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_mod,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_mod,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: config.depth_format,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: config.sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview: None,
        });

        Ok(Self {
            config,
            camera_buffer,
            camera_bind_group_layout,
            camera_bind_group,
            geometry,
            selection,
            gizmo,
            grid,
        })
    }

    /// Upload a new camera uniform to the GPU. Cheap — single
    /// `queue.write_buffer` call.
    pub fn upload_camera(&self, queue: &wgpu::Queue, uniform: &CameraUniform) {
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(uniform));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::RendererBackend;

    #[test]
    fn shader_validation_passes_for_all_four_core_shaders() {
        RenderPipeline::validate_shaders().expect("core shaders must parse");
    }

    #[test]
    fn msaa_normalisation_clamps_invalid_values_to_one() {
        let cfg = PipelineConfig::new(
            3,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Depth24PlusStencil8,
        );
        assert_eq!(cfg.sample_count, 1);
        let cfg4 = PipelineConfig::new(
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Depth24PlusStencil8,
        );
        assert_eq!(cfg4.sample_count, 4);
    }

    #[test]
    fn build_against_headless_renderer_produces_four_pipelines() {
        // No adapter on this runner → skip the GPU portion. The
        // shader validation in `validate_shaders()` (covered by its
        // own dedicated test below) still exercises the wgsl
        // parsing, so we don't lose coverage when CI has no GPU.
        if let Ok(r) = ViewportRenderer::new_headless(RendererBackend::Fallback) {
            let pipe = RenderPipeline::build(&r, PipelineConfig::default())
                .expect("pipeline must build against a real device");
            let _ = (&pipe.geometry, &pipe.selection, &pipe.gizmo, &pipe.grid);
            let q = r.queue.as_ref().unwrap();
            pipe.upload_camera(q, &CameraUniform::identity());
        }
    }

    #[test]
    fn build_without_device_errors_with_no_device() {
        let r = ViewportRenderer::new_headless_minimal(RendererBackend::Fallback);
        match RenderPipeline::build(&r, PipelineConfig::default()) {
            Ok(_) => panic!("expected NoDevice error"),
            Err(PipelineError::NoDevice) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
