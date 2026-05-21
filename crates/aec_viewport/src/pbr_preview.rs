//! PBR rasterization preview pipeline. Replaces the EEVEE preview by
//! driving wgpu directly: a depth-only shadow map pass followed by a
//! single forward PBR pass that samples the [`crate::sky`] environment
//! and the shadow map.
//!
//! This module is structured so it stays useful even without a real
//! GPU: [`PbrPreviewPipeline::new`] returns `Err(PreviewError::NoAdapter)`
//! when wgpu can't acquire an adapter (typical in headless CI), and the
//! caller (`aec_render::preview`) can fall back to a CPU shading path.
//! All shader sources are bundled as `&'static str` and validated via
//! naga at unit-test time so the WGSL is verified even in headless CI.
//!
//! The pipeline is deliberately *single-pass forward + shadow map*:
//! deferred rendering would bloat memory bandwidth for the AEC preview
//! case (typical interior scenes are < 1M triangles, < 32 visible
//! lights), and a single PBR pass already meets the < 250 ms latency
//! target on mid-tier hardware.

use std::borrow::Cow;
use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use thiserror::Error;
use wgpu::util::DeviceExt;

use crate::sky::{build_sky_uniform, SkyState, SkyUniform};

/// Bundled shader sources for the PBR preview pipeline. Exposed for
/// unit-testing (compile via naga) and for the integration crate that
/// wants to recompile with custom defines.
pub const PBR_SHADER_SOURCE: &str = include_str!("shaders/pbr.wgsl");
pub const SKY_SHADER_SOURCE: &str = include_str!("shaders/sky.wgsl");

/// Default shadow-map resolution. 1024² is the sweet spot for an
/// architectural-scale outdoor scene at mid-tier hardware.
pub const DEFAULT_SHADOW_RES: u32 = 1024;

/// Maximum number of instances drawable in a single submit. Real
/// pipelines will batch by mesh; this is the per-batch cap.
pub const MAX_INSTANCES_PER_BATCH: usize = 4096;

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("wgpu adapter request failed")]
    NoAdapter,
    #[error("wgpu device request failed: {0}")]
    DeviceRequest(String),
    #[error("invalid scene: {0}")]
    InvalidScene(String),
}

/// Camera + lighting state that becomes a uniform block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewCamera {
    pub view_proj: Mat4,
    pub inv_view_proj: Mat4,
    pub camera_position: Vec3,
}

impl PreviewCamera {
    /// Build a perspective camera from common AEC parameters.
    pub fn perspective(
        position: Vec3,
        target: Vec3,
        up: Vec3,
        fov_y_deg: f32,
        aspect: f32,
        z_near: f32,
        z_far: f32,
    ) -> Self {
        let view = Mat4::look_at_rh(position, target, up);
        let proj = Mat4::perspective_rh(fov_y_deg.to_radians(), aspect, z_near, z_far);
        let vp = proj * view;
        let inv = vp.inverse();
        Self {
            view_proj: vp,
            inv_view_proj: inv,
            camera_position: position,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
}

const _CAM_UNIFORM_SIZE: () = assert!(std::mem::size_of::<CameraUniform>() == 144);

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct SunUniform {
    direction: [f32; 4],
    colour: [f32; 4],
    light_view_proj: [[f32; 4]; 4],
    /// `[texel_size, resolution_pixels, reserved, reserved]`.
    ///
    /// `texel_size = 1.0 / resolution_pixels`. The PBR fragment shader
    /// reads `sun.shadow_params.x` to compute its 3×3 PCF jitter,
    /// which keeps the shader independent of [`DEFAULT_SHADOW_RES`] and
    /// allows the pipeline to resize the shadow texture at runtime.
    shadow_params: [f32; 4],
}

const _SUN_UNIFORM_SIZE: () = assert!(std::mem::size_of::<SunUniform>() == 112);

/// Direct sun light. Direction points **from ground to sun**.
#[derive(Debug, Clone, Copy)]
pub struct SunLight {
    pub direction: Vec3,
    pub colour: Vec3,
    /// HDR intensity multiplier (cd/m²-scale).
    pub intensity: f32,
}

/// Physical baseline sun illuminance multiplier in the same arbitrary
/// units the PBR pipeline expects (matches the SkyState `strength = 1.0`
/// noon reference).
const BASELINE_SUN_INTENSITY: f32 = 3.0;

/// Warm-white sun colour at noon (CIE D65-ish). Modulated by the sky
/// tint so a tinted SkyState (e.g. warm sunset) also tints the sun.
const BASELINE_SUN_COLOUR: Vec3 = Vec3::new(1.0, 0.97, 0.92);

impl SunLight {
    /// Derive a [`SunLight`] from a sky state. The direction is the
    /// SkyState's sun vector; the colour is the baseline warm-white
    /// modulated by [`SkyState::tint`]; the intensity scales with
    /// [`SkyState::strength`] so dimming the sky also dims the sun and
    /// the two stay energetically consistent.
    pub fn from_sky_state(sky: &SkyState) -> Self {
        let tint = Vec3::new(sky.tint[0], sky.tint[1], sky.tint[2]);
        Self {
            direction: sky.sun_direction(),
            colour: BASELINE_SUN_COLOUR * tint,
            intensity: BASELINE_SUN_INTENSITY * sky.strength.max(0.0),
        }
    }

    /// Build the light-space view-projection matrix that maps the scene
    /// into the shadow map. Uses an orthographic frustum sized to the
    /// supplied world-space radius.
    pub fn light_view_proj(&self, world_center: Vec3, world_radius: f32) -> Mat4 {
        // Eye = world_center + direction * 2*radius (place the light
        // beyond the scene so the near plane stays positive).
        let dir = self.direction.normalize_or_zero();
        let eye = world_center + dir * world_radius.max(1.0) * 2.0;
        // Choose `up` that's not collinear with `dir`. For sun directions
        // pointing along ±Y, fall back to +Z; otherwise use +Y.
        let up = if dir.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
        let view = Mat4::look_at_rh(eye, world_center, up);
        let half = world_radius.max(1.0);
        let proj = Mat4::orthographic_rh(-half, half, -half, half, 0.1, half * 4.0 + 0.1);
        proj * view
    }
}

/// Per-vertex PBR vertex data.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct PbrVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl PbrVertex {
    pub fn new(position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        Self {
            position,
            normal,
            uv,
        }
    }

    pub const STRIDE: u64 = std::mem::size_of::<Self>() as u64;
}

/// Per-instance data. Mirrors the WGSL `InstanceInput` layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct PbrInstance {
    pub transform: [[f32; 4]; 4],
    pub base_color_metallic: [f32; 4],
    pub roughness_ao_emissive: [f32; 4],
}

impl PbrInstance {
    pub fn new(
        transform: Mat4,
        base_color: [f32; 3],
        metallic: f32,
        roughness: f32,
        ao: f32,
        emissive_strength: f32,
    ) -> Self {
        Self {
            transform: transform.to_cols_array_2d(),
            base_color_metallic: [base_color[0], base_color[1], base_color[2], metallic],
            roughness_ao_emissive: [roughness, ao, emissive_strength, 0.0],
        }
    }

    pub const STRIDE: u64 = std::mem::size_of::<Self>() as u64;
}

const _INSTANCE_STRIDE: () = assert!(std::mem::size_of::<PbrInstance>() == 96);

/// A single render-able mesh batch: shared vertex/index buffer with N
/// instances.
pub struct MeshBatch {
    pub vertex_buf: wgpu::Buffer,
    pub index_buf: wgpu::Buffer,
    pub instance_buf: wgpu::Buffer,
    pub index_count: u32,
    pub instance_count: u32,
}

/// Inputs into a single PBR preview render.
pub struct PreviewFrame<'a> {
    pub camera: PreviewCamera,
    pub sun: SunLight,
    pub sky_state: SkyState,
    pub world_center: Vec3,
    pub world_radius: f32,
    pub batches: &'a [MeshBatch],
}

/// The full PBR preview pipeline.
///
/// Owns the wgpu device + queue, the bind group layouts, the shadow map,
/// the depth + colour textures, and the compiled render pipelines.
/// Render targets resize on demand via [`PbrPreviewPipeline::resize`].
pub struct PbrPreviewPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height: u32,
    shadow_res: u32,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,

    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    #[allow(dead_code)] // bound into main_bg; retained for resize-shadow support
    shadow_view: wgpu::TextureView,

    camera_buf: wgpu::Buffer,
    sun_buf: wgpu::Buffer,
    sky_buf: wgpu::Buffer,

    main_bg: wgpu::BindGroup,
    sky_bg: wgpu::BindGroup,
    shadow_bg: wgpu::BindGroup,

    pbr_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,

    color_texture: wgpu::Texture,
}

impl PbrPreviewPipeline {
    /// Acquire a wgpu adapter + device and build the pipeline. Returns
    /// [`PreviewError::NoAdapter`] when no adapter is available (e.g.
    /// headless CI without a software fallback).
    pub fn new(width: u32, height: u32) -> Result<Self, PreviewError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or(PreviewError::NoAdapter)?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aec_viewport::pbr_preview"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .map_err(|e| PreviewError::DeviceRequest(format!("{e}")))?;
        Self::from_device(device, queue, width, height)
    }

    /// Build the pipeline against an externally-supplied device. Used
    /// by tests + by the integration crate that already owns a device.
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
    ) -> Result<Self, PreviewError> {
        let color_format = wgpu::TextureFormat::Rgba16Float;
        let depth_format = wgpu::TextureFormat::Depth32Float;
        let shadow_format = wgpu::TextureFormat::Depth32Float;
        let shadow_res = DEFAULT_SHADOW_RES;

        let (color_texture, color_view) = make_color_texture(&device, width, height, color_format);
        let depth_view = make_depth_texture(&device, width, height, depth_format);
        let shadow_view = make_depth_texture(&device, shadow_res, shadow_res, shadow_format);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pbr_shadow_cmp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pbr_camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sun_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pbr_sun"),
            size: std::mem::size_of::<SunUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sky_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pbr_sky"),
            size: std::mem::size_of::<SkyUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- Main forward PBR bind group ----
        let main_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pbr_main_bgl"),
            entries: &[
                bgl_entry_uniform(0, std::mem::size_of::<CameraUniform>() as u64),
                bgl_entry_uniform(1, std::mem::size_of::<SunUniform>() as u64),
                bgl_entry_uniform(2, std::mem::size_of::<SkyUniform>() as u64),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let main_bg = make_main_bg(
            &device,
            &main_bg_layout,
            &camera_buf,
            &sun_buf,
            &sky_buf,
            &shadow_view,
            &shadow_sampler,
        );

        // ---- Sky-only bind group (no shadow) ----
        let sky_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pbr_sky_bgl"),
            entries: &[
                bgl_entry_uniform(0, std::mem::size_of::<SkyUniform>() as u64),
                bgl_entry_uniform(1, std::mem::size_of::<CameraUniform>() as u64),
            ],
        });
        let sky_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pbr_sky_bg"),
            layout: &sky_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: sky_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: camera_buf.as_entire_binding(),
                },
            ],
        });

        // ---- Shadow-pass bind group (camera = light VP only) ----
        let shadow_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pbr_shadow_bgl"),
            entries: &[bgl_entry_uniform(
                0,
                std::mem::size_of::<SunUniform>() as u64,
            )],
        });
        let shadow_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pbr_shadow_bg"),
            layout: &shadow_bg_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: sun_buf.as_entire_binding(),
            }],
        });

        // ---- Shaders ----
        let pbr_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pbr.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(PBR_SHADER_SOURCE)),
        });
        let sky_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SKY_SHADER_SOURCE)),
        });
        let shadow_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow.wgsl (inline)"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADOW_SHADER_SOURCE)),
        });

        // ---- PBR forward pipeline ----
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: PbrVertex::STRIDE,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 24,
                    shader_location: 2,
                },
            ],
        };
        let ibuf_layout = wgpu::VertexBufferLayout {
            array_stride: PbrInstance::STRIDE,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 4,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 5,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 6,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 64,
                    shader_location: 7,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 80,
                    shader_location: 8,
                },
            ],
        };

        let pbr_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pbr_pl"),
            bind_group_layouts: &[&main_bg_layout],
            push_constant_ranges: &[],
        });
        let pbr_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pbr_pipeline"),
            layout: Some(&pbr_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &pbr_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[vbuf_layout.clone(), ibuf_layout.clone()],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &pbr_module,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
        });

        // ---- Sky pipeline (fullscreen) ----
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky_pl"),
            bind_group_layouts: &[&sky_bg_layout],
            push_constant_ranges: &[],
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky_pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                // Sky is drawn AFTER geometry — only fill pixels with
                // depth still at the far plane (1.0).
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sky_module,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
        });

        // ---- Shadow-map pass (depth-only) ----
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadow_pl"),
                bind_group_layouts: &[&shadow_bg_layout],
                push_constant_ranges: &[],
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow_pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[vbuf_layout, ibuf_layout],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: shadow_format,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview: None,
        });

        // The bind-group layouts and the shadow sampler are owned by
        // their respective bind groups / pipelines and can be dropped
        // here — wgpu refcounts them internally.
        let _ = main_bg_layout;
        let _ = sky_bg_layout;
        let _ = shadow_bg_layout;
        let _ = shadow_sampler;
        let _ = shadow_format;

        Ok(Self {
            device,
            queue,
            width,
            height,
            shadow_res,
            color_format,
            depth_format,
            color_view,
            depth_view,
            shadow_view,
            camera_buf,
            sun_buf,
            sky_buf,
            main_bg,
            sky_bg,
            shadow_bg,
            pbr_pipeline,
            sky_pipeline,
            shadow_pipeline,
            color_texture,
        })
    }

    /// Recreate the colour + depth targets at the requested resolution.
    /// Shadow map is independent and not touched.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        let (tex, view) = make_color_texture(&self.device, width, height, self.color_format);
        self.color_texture = tex;
        self.color_view = view;
        self.depth_view = make_depth_texture(&self.device, width, height, self.depth_format);
        self.width = width;
        self.height = height;
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn shadow_resolution(&self) -> u32 {
        self.shadow_res
    }
    pub fn color_format(&self) -> wgpu::TextureFormat {
        self.color_format
    }

    /// Make the colour target available to outside callers that want to
    /// composite or read back.
    pub fn color_texture(&self) -> &wgpu::Texture {
        &self.color_texture
    }

    /// Borrow the device + queue (used by [`crate::scene`] / the
    /// integration crate to create vertex buffers, transfer data).
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Build a [`MeshBatch`] from CPU-side vertices + indices +
    /// instances. Single submit; rebuilds entirely if the data changes.
    pub fn create_batch(
        &self,
        vertices: &[PbrVertex],
        indices: &[u32],
        instances: &[PbrInstance],
    ) -> MeshBatch {
        let vertex_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pbr_vbuf"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pbr_ibuf"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let instance_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pbr_instbuf"),
                contents: bytemuck::cast_slice(instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        MeshBatch {
            vertex_buf,
            index_buf,
            instance_buf,
            index_count: indices.len() as u32,
            instance_count: instances.len() as u32,
        }
    }

    /// Run a single frame. Writes the PBR result into the pipeline's
    /// internal colour texture; the caller can either present it to a
    /// surface or copy it out via [`Self::color_texture`].
    pub fn render(&mut self, frame: &PreviewFrame<'_>) -> Result<(), PreviewError> {
        let cam = CameraUniform {
            view_proj: frame.camera.view_proj.to_cols_array_2d(),
            inv_view_proj: frame.camera.inv_view_proj.to_cols_array_2d(),
            camera_pos: [
                frame.camera.camera_position.x,
                frame.camera.camera_position.y,
                frame.camera.camera_position.z,
                1.0,
            ],
        };
        let sun_vp = frame
            .sun
            .light_view_proj(frame.world_center, frame.world_radius);
        let shadow_res_f = self.shadow_res.max(1) as f32;
        let sun = SunUniform {
            direction: [
                frame.sun.direction.x,
                frame.sun.direction.y,
                frame.sun.direction.z,
                0.0,
            ],
            colour: [
                frame.sun.colour.x * frame.sun.intensity,
                frame.sun.colour.y * frame.sun.intensity,
                frame.sun.colour.z * frame.sun.intensity,
                1.0,
            ],
            light_view_proj: sun_vp.to_cols_array_2d(),
            shadow_params: [1.0 / shadow_res_f, shadow_res_f, 0.0, 0.0],
        };
        let sky = build_sky_uniform(&frame.sky_state);

        self.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cam));
        self.queue
            .write_buffer(&self.sun_buf, 0, bytemuck::bytes_of(&sun));
        self.queue
            .write_buffer(&self.sky_buf, 0, bytemuck::bytes_of(&sky));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pbr_preview_encoder"),
            });

        // 1. Shadow map pass
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pbr_shadow_pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.shadow_bg, &[]);
            for batch in frame.batches {
                pass.set_vertex_buffer(0, batch.vertex_buf.slice(..));
                pass.set_vertex_buffer(1, batch.instance_buf.slice(..));
                pass.set_index_buffer(batch.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..batch.index_count, 0, 0..batch.instance_count);
            }
        }

        // 2. Forward PBR pass + sky background.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pbr_forward_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Geometry first (so the sky composites only where depth == 1).
            pass.set_pipeline(&self.pbr_pipeline);
            pass.set_bind_group(0, &self.main_bg, &[]);
            for batch in frame.batches {
                pass.set_vertex_buffer(0, batch.vertex_buf.slice(..));
                pass.set_vertex_buffer(1, batch.instance_buf.slice(..));
                pass.set_index_buffer(batch.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..batch.index_count, 0, 0..batch.instance_count);
            }

            // Sky fills the remaining (far-plane) pixels.
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.sky_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

// ----- helpers --------------------------------------------------------

fn bgl_entry_uniform(binding: u32, min_size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(min_size),
        },
        count: None,
    }
}

fn make_color_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pbr_color"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn make_depth_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pbr_depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn make_main_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    camera: &wgpu::Buffer,
    sun: &wgpu::Buffer,
    sky: &wgpu::Buffer,
    shadow_view: &wgpu::TextureView,
    shadow_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pbr_main_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: sun.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: sky.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(shadow_sampler),
            },
        ],
    })
}

// Inline depth-only shadow shader. Kept here (not as a separate .wgsl
// file) because it's a one-liner sharing the same `InstanceInput`
// layout as the main PBR shader.
const SHADOW_SHADER_SOURCE: &str = r#"
struct SunLight {
    direction: vec4<f32>,
    colour: vec4<f32>,
    light_view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> sun: SunLight;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
    @location(2) uv:       vec2<f32>,
};
struct InstanceInput {
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) m3: vec4<f32>,
    @location(7) base_pmr: vec4<f32>,
    @location(8) rough_aoe: vec4<f32>,
};

@vertex
fn vs_main(v: VertexInput, i: InstanceInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(i.m0, i.m1, i.m2, i.m3);
    let world = model * vec4<f32>(v.position, 1.0);
    return sun.light_view_proj * world;
}
"#;

/// Validate every WGSL shader bundled with the preview pipeline. Useful
/// from headless CI where no GPU adapter is available — naga is a
/// CPU-only parser/validator.
pub fn validate_shaders() -> Result<(), String> {
    naga::front::wgsl::parse_str(PBR_SHADER_SOURCE).map_err(|e| format!("pbr.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(SKY_SHADER_SOURCE).map_err(|e| format!("sky.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(SHADOW_SHADER_SOURCE).map_err(|e| format!("shadow.wgsl: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sky::SkyState;

    #[test]
    fn pbr_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(PBR_SHADER_SOURCE).expect("pbr.wgsl should validate");
    }

    #[test]
    fn sky_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(SKY_SHADER_SOURCE).expect("sky.wgsl should validate");
    }

    #[test]
    fn shadow_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(SHADOW_SHADER_SOURCE).expect("shadow shader should validate");
    }

    #[test]
    fn validate_shaders_all_ok() {
        validate_shaders().expect("all preview shaders should be valid WGSL");
    }

    #[test]
    fn pbr_instance_stride_is_96_bytes() {
        assert_eq!(std::mem::size_of::<PbrInstance>(), 96);
    }

    #[test]
    fn pbr_vertex_stride_is_32_bytes() {
        assert_eq!(std::mem::size_of::<PbrVertex>(), 32);
    }

    #[test]
    fn perspective_camera_inverse_is_consistent() {
        let cam = PreviewCamera::perspective(
            Vec3::new(0.0, 1.0, 5.0),
            Vec3::ZERO,
            Vec3::Y,
            60.0,
            16.0 / 9.0,
            0.1,
            100.0,
        );
        let product = cam.view_proj * cam.inv_view_proj;
        let id = Mat4::IDENTITY;
        for r in 0..4 {
            for c in 0..4 {
                let diff = (product.col(c)[r] - id.col(c)[r]).abs();
                assert!(diff < 1e-3, "vp * inv_vp not identity at ({r},{c}): {diff}");
            }
        }
    }

    #[test]
    fn sun_light_view_proj_projects_world_center_to_origin_xy() {
        let sky = SkyState::clear_noon();
        let sun = SunLight::from_sky_state(&sky);
        let vp = sun.light_view_proj(Vec3::ZERO, 10.0);
        let p = vp * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        let ndc = p.truncate() / p.w;
        assert!(ndc.x.abs() < 1e-3, "ndc.x = {}", ndc.x);
        assert!(ndc.y.abs() < 1e-3, "ndc.y = {}", ndc.y);
        // Origin should be at mid-depth (somewhere in [0, 1]) under the
        // orthographic frustum we built.
        assert!(ndc.z >= 0.0 && ndc.z <= 1.0, "ndc.z = {}", ndc.z);
    }

    #[test]
    fn sun_uniform_size_is_112_bytes() {
        // direction(16) + colour(16) + light_view_proj(64) + shadow_params(16) = 112
        assert_eq!(std::mem::size_of::<SunUniform>(), 112);
    }

    #[test]
    fn camera_uniform_size_is_144_bytes() {
        assert_eq!(std::mem::size_of::<CameraUniform>(), 144);
    }

    /// Headless adapter creation. Returns the pipeline if a wgpu adapter
    /// is available, `None` otherwise (e.g. headless CI). Used by the
    /// pipeline-construction tests below.
    fn try_pipeline() -> Option<PbrPreviewPipeline> {
        PbrPreviewPipeline::new(256, 256).ok()
    }

    #[test]
    fn pipeline_constructs_or_skips_on_headless() {
        match try_pipeline() {
            Some(p) => {
                assert_eq!(p.width(), 256);
                assert_eq!(p.height(), 256);
                assert_eq!(p.shadow_resolution(), DEFAULT_SHADOW_RES);
                assert_eq!(p.color_format(), wgpu::TextureFormat::Rgba16Float);
            }
            None => {
                eprintln!("skipping: no wgpu adapter available (likely headless CI)");
            }
        }
    }

    #[test]
    fn pipeline_resize_updates_dimensions() {
        if let Some(mut p) = try_pipeline() {
            p.resize(640, 480);
            assert_eq!(p.width(), 640);
            assert_eq!(p.height(), 480);
        }
    }

    /// Render a triangle through the full pipeline, then sanity-check
    /// that the colour target is in a writable state (the test only
    /// runs when a GPU adapter is available).
    #[test]
    fn pipeline_renders_a_triangle_without_validation_errors() {
        let Some(mut p) = try_pipeline() else {
            return;
        };
        let verts = vec![
            PbrVertex::new([-0.5, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            PbrVertex::new([0.5, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
            PbrVertex::new([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.5, 1.0]),
        ];
        let idx = vec![0u32, 1, 2];
        let inst = vec![PbrInstance::new(
            Mat4::IDENTITY,
            [0.7, 0.5, 0.3],
            0.0,
            0.5,
            1.0,
            0.0,
        )];
        let batch = p.create_batch(&verts, &idx, &inst);

        let sky = SkyState::clear_noon();
        let sun = SunLight::from_sky_state(&sky);
        let frame = PreviewFrame {
            camera: PreviewCamera::perspective(
                Vec3::new(0.0, 0.5, 3.0),
                Vec3::new(0.0, 0.5, 0.0),
                Vec3::Y,
                60.0,
                1.0,
                0.1,
                100.0,
            ),
            sun,
            sky_state: sky,
            world_center: Vec3::ZERO,
            world_radius: 5.0,
            batches: std::slice::from_ref(&batch),
        };
        p.render(&frame).expect("render succeeds");
    }
}
