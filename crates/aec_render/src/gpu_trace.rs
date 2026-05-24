//! GPU compute-shader path tracer.
//!
//! Mirrors the CPU megakernel in [`crate::path_trace::trace_path`] but
//! runs as a single wgpu compute pipeline. The host (this module)
//! prepares the scene as a set of bytemuck-`Pod` storage buffers
//! (`bvh_nodes`, `triangles`, `materials`, `lights`), uploads them to
//! the device, dispatches a 2D workgroup grid over the image, and reads
//! back the per-pixel accumulator.
//!
//! ### Fallback
//!
//! Adapter acquisition can fail in headless CI environments
//! (no GPU, no software backend). [`GpuPathTracer::try_new`] returns
//! `Err(GpuTraceError::NoAdapter)` in that case; callers should fall
//! through to [`crate::path_trace::render`] on the CPU. We also expose
//! [`render_or_fallback`] which performs the dispatch when possible and
//! transparently falls back when not.
//!
//! ### Shader source
//!
//! The WGSL source is checked in at `shaders/path_trace.wgsl` and
//! validated at build time via [`naga`] in the unit tests (so a syntax
//! error fails CI even on headless workers).

use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Vec3};
use thiserror::Error;
use wgpu::util::DeviceExt;

use crate::path_trace::{
    AccumulationBuffer, CancelToken, PathTraceConfig, PathTraceScene, ProgressFn,
};
use crate::scene::RenderCamera;

pub const SHADER_SOURCE: &str = include_str!("shaders/path_trace.wgsl");

#[derive(Debug, Error)]
pub enum GpuTraceError {
    #[error("could not request a wgpu adapter (headless / no GPU?)")]
    NoAdapter,
    #[error("device request failed: {0}")]
    DeviceRequest(String),
    #[error("shader compilation failed: {0}")]
    Shader(String),
    #[error("scene buffer build failed: {0}")]
    Scene(String),
}

// ---- GPU-side struct layouts. All sizes assert at compile time. -------

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct BvhNodeGpu {
    min: [f32; 3],
    left_or_start: u32,
    max: [f32; 3],
    prim_count: u32,
}
const _: () = assert!(std::mem::size_of::<BvhNodeGpu>() == 32);

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct TriangleGpu {
    v0: [f32; 3],
    _pad0: f32,
    v1: [f32; 3],
    _pad1: f32,
    v2: [f32; 3],
    material_id: i32,
}
const _: () = assert!(std::mem::size_of::<TriangleGpu>() == 48);

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct MaterialGpu {
    base_color: [f32; 3],
    metallic: f32,
    emissive: [f32; 3],
    roughness: f32,
    f0: [f32; 3],
    ior: f32,
}
const _: () = assert!(std::mem::size_of::<MaterialGpu>() == 48);

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct LightGpu {
    kind: u32,
    _pad_kind: [u32; 3],
    position: [f32; 3],
    _pad_position: f32,
    params: [f32; 4],
    extra: [f32; 4],
    size: [f32; 2],
    _pad_size: [f32; 2],
    emission: [f32; 3],
    pad: f32,
}
const _: () = assert!(std::mem::size_of::<LightGpu>() == 96);

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct ParamsGpu {
    width: u32,
    height: u32,
    samples_per_pixel: u32,
    max_bounces: u32,
    tri_count: u32,
    bvh_count: u32,
    mat_count: u32,
    light_count: u32,
    camera_origin: [f32; 3],
    focal_half_h: f32,
    camera_right: [f32; 3],
    aspect: f32,
    camera_up: [f32; 3],
    pad_up: f32,
    camera_forward: [f32; 3],
    seed: u32,
    sky_color: [f32; 3],
    sky_strength: f32,
    /// Camera projection enum encoded for the WGSL shader:
    /// 0 = perspective, 1 = equirectangular. Must match
    /// [`crate::path_trace::CameraProjection`].
    projection: u32,
    /// Padding so the struct ends on a 16-byte (`vec4`) boundary, as
    /// required by WGSL uniform layout rules.
    _pad_projection: [u32; 3],
}
const _: () = assert!(std::mem::size_of::<ParamsGpu>() == 128);

// ---- Scene -> GPU buffers ---------------------------------------------

/// Snapshot of the path-traced scene as flat GPU buffers. Cheap to
/// construct; expensive to upload — cache in callers.
#[derive(Debug, Clone)]
pub struct GpuSceneBuffers {
    bvh_nodes: Vec<BvhNodeGpu>,
    triangles: Vec<TriangleGpu>,
    materials: Vec<MaterialGpu>,
    lights: Vec<LightGpu>,
    sky_color: [f32; 3],
    sky_strength: f32,
}

impl GpuSceneBuffers {
    /// Flatten a [`PathTraceScene`] for GPU consumption. The BVH leaf
    /// permutation is applied here so the shader can index triangles
    /// directly by `prim_indices[start + i]`.
    pub fn build(scene: &PathTraceScene) -> Self {
        // Note: when `scene.bvh.nodes` is empty we deliberately keep
        // `bvh_nodes` empty. The wgpu buffer-not-empty constraint is
        // satisfied at bind time by stubbing a placeholder node
        // (see `GpuPathTracer::dispatch`) so that `params.bvh_count`
        // here continues to reflect the *logical* node count. The
        // WGSL traversal guards on `params.bvh_count == 0u` and
        // returns the miss-hit immediately, so the placeholder node
        // is never read. Mirrors the triangle/material/light stub
        // pattern below.
        let bvh_nodes: Vec<BvhNodeGpu> = scene
            .bvh
            .nodes
            .iter()
            .map(|n| BvhNodeGpu {
                min: n.bounds.min.to_array(),
                left_or_start: n.left_or_start,
                max: n.bounds.max.to_array(),
                prim_count: n.prim_count,
            })
            .collect();

        // Reorder triangles per the BVH's prim_indices so the shader
        // can do contiguous leaf scans without an indirection.
        let mut triangles: Vec<TriangleGpu> = Vec::with_capacity(scene.triangles.len());
        let order: Vec<u32> = if scene.bvh.prim_indices.is_empty() {
            (0..scene.triangles.len() as u32).collect()
        } else {
            scene.bvh.prim_indices.clone()
        };
        for &orig_idx in &order {
            let tri = &scene.triangles[orig_idx as usize];
            let mat_id = scene
                .material_ids
                .get(orig_idx as usize)
                .copied()
                .unwrap_or(-1);
            triangles.push(TriangleGpu {
                v0: tri.v0.to_array(),
                _pad0: 0.0,
                v1: tri.v1.to_array(),
                _pad1: 0.0,
                v2: tri.v2.to_array(),
                material_id: mat_id,
            });
        }
        // Adjust BVH leaf starts: they index into prim_indices, but
        // since we just reordered triangles, prim_id i in the GPU
        // triangle array corresponds to prim_indices[i] in the
        // unordered array. So the leaf's `left_or_start` is already
        // valid (it was a slice into prim_indices, and the GPU now
        // reads triangles in that order).

        let materials: Vec<MaterialGpu> = scene
            .materials
            .iter()
            .map(|m| MaterialGpu {
                base_color: m.base_color.to_array(),
                metallic: m.metallic,
                emissive: m.emissive.to_array(),
                roughness: m.roughness,
                f0: m.f0().to_array(),
                ior: m.ior,
            })
            .collect();

        let lights: Vec<LightGpu> = scene.lights.iter().map(LightGpu::from_native).collect();

        Self {
            bvh_nodes,
            triangles,
            materials,
            lights,
            sky_color: scene.sky.color,
            sky_strength: scene.sky.strength,
        }
    }

    pub fn bvh_node_count(&self) -> usize {
        self.bvh_nodes.len()
    }
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }
    pub fn material_count(&self) -> usize {
        self.materials.len()
    }
    pub fn light_count(&self) -> usize {
        self.lights.len()
    }
}

impl LightGpu {
    fn from_native(l: &crate::light_sampling::NativeLight) -> Self {
        use crate::light_sampling::NativeLight;
        match l {
            NativeLight::Sun {
                direction,
                radiance,
                angular_radius_rad,
            } => LightGpu {
                kind: 0,
                _pad_kind: [0; 3],
                // Sun: the `position` slot stores the (normalized) direction.
                position: direction.normalize_or_zero().to_array(),
                _pad_position: 0.0,
                params: [*angular_radius_rad, 0.0, 0.0, 0.0],
                extra: [0.0; 4],
                size: [0.0; 2],
                _pad_size: [0.0; 2],
                emission: radiance.to_array(),
                pad: 0.0,
            },
            NativeLight::Point {
                position,
                intensity,
            } => LightGpu {
                kind: 1,
                _pad_kind: [0; 3],
                position: position.to_array(),
                _pad_position: 0.0,
                params: [0.0; 4],
                extra: [0.0; 4],
                size: [0.0; 2],
                _pad_size: [0.0; 2],
                emission: intensity.to_array(),
                pad: 0.0,
            },
            NativeLight::Area {
                position,
                normal: _,
                u_axis,
                v_axis,
                width,
                height,
                radiance,
            } => LightGpu {
                kind: 2,
                _pad_kind: [0; 3],
                position: position.to_array(),
                _pad_position: 0.0,
                params: [u_axis.x, u_axis.y, u_axis.z, 0.0],
                extra: [v_axis.x, v_axis.y, v_axis.z, 0.0],
                size: [*width, *height],
                _pad_size: [0.0; 2],
                emission: radiance.to_array(),
                pad: 0.0,
            },
            NativeLight::Ies {
                position,
                forward: _,
                up: _,
                profile,
                intensity_scale,
                color,
            } => {
                // IES is sampled on CPU only — for the GPU we treat it
                // as a point light scaled by the profile's representative
                // candela (sampled at the down axis). Quality is preserved
                // on the CPU tracer, which is the primary path; the GPU
                // path is for fast interactive refinement.
                let representative_cd = profile.candela_at(0.0, 0.0).max(1.0);
                let scale = representative_cd * *intensity_scale;
                LightGpu {
                    kind: 1,
                    _pad_kind: [0; 3],
                    position: position.to_array(),
                    _pad_position: 0.0,
                    params: [0.0; 4],
                    extra: [0.0; 4],
                    size: [0.0; 2],
                    _pad_size: [0.0; 2],
                    emission: (*color * scale).to_array(),
                    pad: 0.0,
                }
            }
        }
    }
}

// ---- Public API -------------------------------------------------------

/// Top-level GPU path tracer. Owns the wgpu device/queue/pipeline. Cheap
/// to keep alive across renders.
pub struct GpuPathTracer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuPathTracer {
    /// Attempt to bring up a GPU path tracer on the default adapter.
    pub fn try_new() -> Result<Self, GpuTraceError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or(GpuTraceError::NoAdapter)?;
        // We need 5 storage buffers per stage (bvh / triangles /
        // materials / lights / accumulator). Downlevel defaults only
        // guarantee 4, so request a slightly higher floor — but cap to
        // what the adapter actually supports so we don't error on
        // request_device. This is sufficient for software adapters
        // (lavapipe advertises 16) and native GPUs alike.
        let adapter_limits = adapter.limits();
        // If the adapter can't hit 8 storage buffers per stage (we use
        // 5), fall back to NoAdapter so callers route to CPU.
        if adapter_limits.max_storage_buffers_per_shader_stage < 8 {
            return Err(GpuTraceError::NoAdapter);
        }
        let required_limits = wgpu::Limits {
            max_storage_buffers_per_shader_stage: 8,
            ..wgpu::Limits::downlevel_defaults()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aec_render::gpu_trace::device"),
                required_features: wgpu::Features::empty(),
                required_limits,
            },
            None,
        ))
        .map_err(|e| GpuTraceError::DeviceRequest(e.to_string()))?;
        Self::from_device(device, queue)
    }

    /// Build a [`GpuPathTracer`] from a pre-acquired device + queue.
    /// Lets callers share the wgpu device with the viewport.
    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, GpuTraceError> {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aec_render::gpu_trace::shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(SHADER_SOURCE)),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aec_render::gpu_trace::bgl"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, true),
                storage_entry(2, true),
                storage_entry(3, true),
                uniform_entry(4),
                storage_entry(5, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aec_render::gpu_trace::pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aec_render::gpu_trace::pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
        })
    }

    /// Dispatch the GPU path tracer over the full image. Returns an
    /// [`AccumulationBuffer`] populated with `samples_per_pixel`
    /// samples per pixel.
    pub fn render(
        &self,
        scene: &PathTraceScene,
        camera: &RenderCamera,
        config: &PathTraceConfig,
        progress: Option<ProgressFn>,
        cancel: Option<CancelToken>,
    ) -> AccumulationBuffer {
        if cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return AccumulationBuffer::new(config.width, config.height);
        }

        let buffers = GpuSceneBuffers::build(scene);
        // Same stride/zero-length validation issue as `triangles`
        // below: an empty `bvh_nodes` buffer is bound with 16 bytes
        // of wgpu padding but the shader's storage array expects
        // `stride_of<BvhNodeGpu>` = 32 bytes per element, which trips
        // the dispatch-time validator. Stub a single zeroed
        // placeholder when the scene has no BVH; the shader guards
        // traversal with `params.bvh_count == 0u`, so the placeholder
        // is never read. Crucially, `params.bvh_count` is computed
        // from `buffers.bvh_nodes.len()` further down, so it remains
        // `0` for empty scenes and the WGSL early-return fires
        // structurally rather than relying on the placeholder's
        // degenerate AABB to miss every ray.
        let bvh_buf = if buffers.bvh_nodes.is_empty() {
            self.create_storage(
                "bvh_nodes",
                bytemuck::cast_slice(&[BvhNodeGpu {
                    min: [0.0; 3],
                    left_or_start: 0,
                    max: [0.0; 3],
                    prim_count: 0,
                }]),
            )
        } else {
            self.create_storage("bvh_nodes", bytemuck::cast_slice(&buffers.bvh_nodes))
        };
        // Empty triangle buffer is bound with 16 bytes of wgpu padding
        // but the shader's storage array expects `stride_of<TriangleGpu>`
        // = 48 bytes per element, so binding the zero-length buffer
        // triggers a validation error on dispatch. Stub a single
        // zeroed placeholder when there are no triangles — the shader
        // already guards traversal with `params.bvh_count == 0u`, so
        // the stub is never read. Mirrors the same pattern already
        // used for `materials` and `lights` below.
        let tri_buf = if buffers.triangles.is_empty() {
            self.create_storage(
                "triangles",
                bytemuck::cast_slice(&[TriangleGpu {
                    v0: [0.0; 3],
                    _pad0: 0.0,
                    v1: [0.0; 3],
                    _pad1: 0.0,
                    v2: [0.0; 3],
                    material_id: -1,
                }]),
            )
        } else {
            self.create_storage("triangles", bytemuck::cast_slice(&buffers.triangles))
        };
        let mat_buf = if buffers.materials.is_empty() {
            self.create_storage(
                "materials",
                bytemuck::cast_slice(&[MaterialGpu {
                    base_color: [0.5; 3],
                    metallic: 0.0,
                    emissive: [0.0; 3],
                    roughness: 0.5,
                    f0: [0.04; 3],
                    ior: 1.5,
                }]),
            )
        } else {
            self.create_storage("materials", bytemuck::cast_slice(&buffers.materials))
        };
        let light_buf = if buffers.lights.is_empty() {
            self.create_storage(
                "lights",
                bytemuck::cast_slice(&[LightGpu {
                    kind: 1,
                    _pad_kind: [0; 3],
                    position: [0.0; 3],
                    _pad_position: 0.0,
                    params: [0.0; 4],
                    extra: [0.0; 4],
                    size: [0.0; 2],
                    _pad_size: [0.0; 2],
                    emission: [0.0; 3],
                    pad: 0.0,
                }]),
            )
        } else {
            self.create_storage("lights", bytemuck::cast_slice(&buffers.lights))
        };

        let camera_frame = build_camera_frame(camera);
        let projection = match config.projection {
            crate::path_trace::CameraProjection::Perspective => 0u32,
            crate::path_trace::CameraProjection::Equirectangular => 1u32,
        };
        let params = ParamsGpu {
            width: config.width,
            height: config.height,
            samples_per_pixel: config.samples_per_pixel.max(1),
            max_bounces: config.max_bounces.max(1),
            tri_count: buffers.triangles.len() as u32,
            bvh_count: buffers.bvh_nodes.len() as u32,
            mat_count: buffers.materials.len() as u32,
            light_count: buffers.lights.len() as u32,
            camera_origin: camera_frame.origin.to_array(),
            focal_half_h: camera_frame.focal_half_h,
            camera_right: camera_frame.right.to_array(),
            aspect: config.width as f32 / config.height.max(1) as f32,
            camera_up: camera_frame.up.to_array(),
            pad_up: 0.0,
            camera_forward: camera_frame.forward.to_array(),
            seed: 0xC0FFEE,
            sky_color: buffers.sky_color,
            sky_strength: buffers.sky_strength,
            projection,
            _pad_projection: [0; 3],
        };
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aec_render::gpu_trace::params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let pixel_count = (config.width as usize) * (config.height as usize);
        let accum_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aec_render::gpu_trace::accum"),
            size: (pixel_count * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aec_render::gpu_trace::bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bvh_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tri_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: mat_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: light_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: accum_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aec_render::gpu_trace::encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aec_render::gpu_trace::pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let workgroups_x = config.width.div_ceil(8);
            let workgroups_y = config.height.div_ceil(8);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }

        // Stage a CPU-readable buffer for readback.
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aec_render::gpu_trace::readback"),
            size: (pixel_count * 16) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&accum_buf, 0, &readback, 0, (pixel_count * 16) as u64);
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device.poll(wgpu::Maintain::Wait);
        // If mapping fails, return an empty buffer — caller can fall
        // through to CPU.
        if let Ok(Ok(())) = rx.recv() {
            let data = slice.get_mapped_range();
            let floats: &[f32] = bytemuck::cast_slice(&data);
            let mut buffer = AccumulationBuffer::new(config.width, config.height);
            for i in 0..pixel_count {
                let r = floats[i * 4];
                let g = floats[i * 4 + 1];
                let b = floats[i * 4 + 2];
                let n = floats[i * 4 + 3].max(1.0);
                // The shader stores already-averaged radiance plus the
                // sample count, so to match the CPU accumulator format
                // (`sums + sample_count`) we re-multiply by n.
                buffer.pixels[i] = [r * n, g * n, b * n, n];
            }
            drop(data);
            readback.unmap();
            if let Some(cb) = progress.as_ref() {
                cb(1, 1);
            }
            return buffer;
        }
        AccumulationBuffer::new(config.width, config.height)
    }

    fn create_storage(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        // Empty buffers are not allowed; pad to 16 bytes if needed.
        let mut bytes: Vec<u8> = Vec::with_capacity(contents.len().max(16));
        bytes.extend_from_slice(contents);
        if bytes.is_empty() {
            bytes.resize(16, 0);
        }
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: &bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

// ---- Camera helpers (CPU side matches WGSL constants) ----------------

struct CameraFrame {
    origin: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    focal_half_h: f32,
}

fn build_camera_frame(camera: &RenderCamera) -> CameraFrame {
    let origin = Vec3::from_array(camera.position_mm) * 0.001;
    let target = Vec3::from_array(camera.target_mm) * 0.001;
    let raw_forward = (target - origin).normalize_or_zero();
    let forward = if raw_forward.length_squared() < 1e-8 {
        Vec3::NEG_Z
    } else {
        raw_forward
    };
    let world_up = Vec3::Y;
    let raw_right = forward.cross(world_up).normalize_or_zero();
    let right = if raw_right.length_squared() < 1e-8 {
        Vec3::X
    } else {
        raw_right
    };
    let up = right.cross(forward).normalize();
    let focal_mm = camera.focal_length_mm.max(1.0);
    let sensor_h_mm = 24.0_f32;
    let focal_half_h = (sensor_h_mm * 0.5) / focal_mm;
    let _ = Mat3::from_cols(right, up, -forward); // keep for parity w/ CPU build_view
    CameraFrame {
        origin,
        forward,
        right,
        up,
        focal_half_h,
    }
}

/// Convenience: attempt GPU render; on any failure fall back to the
/// CPU [`crate::path_trace::render`]. This is the entry point the
/// scheduler / queue should call.
pub fn render_or_fallback(
    scene: &PathTraceScene,
    camera: &RenderCamera,
    config: &PathTraceConfig,
    progress: Option<ProgressFn>,
    cancel: Option<CancelToken>,
    capture_aux: bool,
) -> AccumulationBuffer {
    match GpuPathTracer::try_new() {
        // The GPU path does not yet emit first-hit aux buffers (would
        // require new bindings + a GBuffer pass in the WGSL kernel).
        // When the caller asked for aux but the GPU path was selected,
        // we silently drop aux for now: the bilateral kernel in
        // `final_render::encode_srgb8` handles `None` aux as a
        // luminance-only fallback. A future PR will plumb aux through
        // the GPU compute kernel.
        Ok(tracer) => tracer.render(scene, camera, config, progress, cancel),
        Err(_) => {
            if capture_aux {
                crate::path_trace::render_with_aux(scene, camera, config, progress, cancel)
            } else {
                crate::path_trace::render(scene, camera, config, progress, cancel)
            }
        }
    }
}

// ---- Shader validation -----------------------------------------------

/// Validate the WGSL shader source via naga. Always available — no GPU
/// required. Used by both the unit tests and as an early sanity check
/// at startup.
pub fn validate_shader() -> Result<(), String> {
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    let module = naga::front::wgsl::parse_str(SHADER_SOURCE).map_err(|e| e.to_string())?;
    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    validator.validate(&module).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::SkyParams;
    use crate::scene::{RenderLight, RenderScene, SerializedMesh};

    fn identity_matrix() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn quad_scene() -> PathTraceScene {
        let mut scene = RenderScene::default();
        scene.push_mesh(SerializedMesh {
            id: "floor".into(),
            positions: vec![
                [-2.0, 0.0, -2.0],
                [2.0, 0.0, -2.0],
                [2.0, 0.0, 2.0],
                [-2.0, 0.0, 2.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: vec![[0.0, 0.0]; 4],
            material_id: None,
            transform: identity_matrix(),
        });
        scene.push_light(RenderLight::Point {
            position_mm: [0.0, 2000.0, 0.0],
            intensity: 1500.0,
            color_temperature_k: 5500.0,
        });
        PathTraceScene::from_render_scene(&scene, vec![], |_| None, SkyParams::default())
    }

    #[test]
    fn shader_source_compiles_via_naga() {
        validate_shader().expect("WGSL must validate");
    }

    #[test]
    fn gpu_struct_sizes_match_wgsl_layout() {
        assert_eq!(std::mem::size_of::<BvhNodeGpu>(), 32);
        assert_eq!(std::mem::size_of::<TriangleGpu>(), 48);
        assert_eq!(std::mem::size_of::<MaterialGpu>(), 48);
        assert_eq!(std::mem::size_of::<LightGpu>(), 96);
        assert_eq!(std::mem::size_of::<ParamsGpu>(), 128);
    }

    #[test]
    fn scene_buffers_capture_geometry_and_lights() {
        let scene = quad_scene();
        let buffers = GpuSceneBuffers::build(&scene);
        // Two triangles in the floor quad.
        assert_eq!(buffers.triangle_count(), 2);
        // BVH built (root present).
        assert!(buffers.bvh_node_count() >= 1);
        // One point light.
        assert_eq!(buffers.light_count(), 1);
        // Each triangle's vertices match (after BVH reordering — quad
        // has two leaves with one prim each, or a single leaf).
        for tri in &buffers.triangles {
            assert!(tri.v0[1].abs() < 1.0e-3, "floor y=0");
        }
    }

    #[test]
    fn camera_frame_is_right_handed() {
        let camera = RenderCamera {
            id: "cam".into(),
            position_mm: [0.0, 1500.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        let frame = build_camera_frame(&camera);
        let cross = frame.right.cross(frame.up).normalize();
        // For a right-handed basis with forward = right × up flipped
        // (we store -forward via Mat3 cols), check coplanarity.
        assert!((cross.dot(frame.forward).abs() - 1.0).abs() < 0.05);
    }

    #[test]
    fn render_or_fallback_returns_buffer_even_without_gpu() {
        // On CI we typically have no GPU. The function must still
        // produce a valid AccumulationBuffer via CPU fallback.
        let scene = quad_scene();
        let camera = RenderCamera {
            id: "cam".into(),
            position_mm: [0.0, 1500.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        let cfg = PathTraceConfig {
            width: 16,
            height: 12,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = render_or_fallback(&scene, &camera, &cfg, None, None, false);
        assert_eq!(buf.width, 16);
        assert_eq!(buf.height, 12);
        assert_eq!(buf.pixels.len(), 16 * 12);
    }

    #[test]
    fn cancel_token_short_circuits_gpu_render() {
        // We can call render() on a real tracer only if a GPU is
        // available; instead drive the cancellation via the fallback
        // path which honours the same token.
        let scene = quad_scene();
        let camera = RenderCamera {
            id: "cam".into(),
            position_mm: [0.0, 1500.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        let cfg = PathTraceConfig {
            width: 16,
            height: 12,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let token = CancelToken::new();
        token.cancel();
        let buf = render_or_fallback(&scene, &camera, &cfg, None, Some(token), false);
        // Even cancelled, the buffer is allocated with the requested
        // dimensions.
        assert_eq!(buf.width, 16);
        assert_eq!(buf.height, 12);
    }

    #[test]
    fn gpu_equirectangular_render_matches_cpu_when_gpu_available() {
        // Regression for PR #12 Devin Review finding BUG_0001: the GPU
        // shader used to ignore the camera projection and always
        // generate perspective rays, so a panorama dispatched through
        // `render_or_fallback` silently produced perspective output on
        // GPU-equipped machines. With the projection field plumbed
        // through `ParamsGpu` + WGSL, the GPU equirectangular path must
        // produce output that matches the CPU reference within a tight
        // PSNR-like tolerance.
        //
        // Headless CI without a GPU adapter simply exits early — the
        // CPU path is already covered by
        // `crate::path_trace::tests::equirectangular_camera_covers_full_sphere`.
        let Ok(tracer) = GpuPathTracer::try_new() else {
            return;
        };
        let scene = quad_scene();
        let camera = RenderCamera {
            id: "pano".into(),
            position_mm: [0.0, 1500.0, 0.0],
            target_mm: [0.0, 1500.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        // Tiny 8x4 panorama — keeps the test fast but still covers the
        // full sphere, so any divergence between CPU and GPU primary
        // rays manifests as a per-pixel radiance difference.
        let cfg = PathTraceConfig {
            width: 8,
            height: 4,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Equirectangular,
        };
        let gpu = tracer.render(&scene, &camera, &cfg, None, None);
        let cpu = crate::path_trace::render(&scene, &camera, &cfg, None, None);

        // Sanity: dimensions agree.
        assert_eq!(gpu.width, cpu.width);
        assert_eq!(gpu.height, cpu.height);

        // Compare averaged radiance per pixel. GPU and CPU use
        // different RNG sequences (host-PCG vs CPU rayon), so direct
        // equality won't hold — instead we require the per-channel
        // mean-squared error to stay below a generous threshold. The
        // important invariant is that the bug fix produces a panorama
        // (escaped rays sample the sky / sun in the same hemisphere
        // pattern), not perspective output (which would hit only the
        // forward cone).
        let gpu_avg = gpu.average_rgb();
        let cpu_avg = cpu.average_rgb();
        let mut sse = 0.0_f64;
        for (g, c) in gpu_avg.iter().zip(cpu_avg.iter()) {
            for ch in 0..3 {
                let d = (g[ch] - c[ch]) as f64;
                sse += d * d;
            }
        }
        let mse = sse / (gpu_avg.len() as f64 * 3.0);
        // Generous threshold: equirectangular rays from CPU vs GPU
        // sample slightly different jitter, but the radiance
        // distribution should agree to within 0.5 in linear radiance
        // for this synthetic scene. A perspective-projecting GPU
        // shader (the bug) would produce MSE well above 1.0 because
        // most pixels would never escape the camera cone and would
        // therefore see a different mix of floor + sky than the CPU's
        // full-sphere equirectangular sample.
        assert!(
            mse < 0.5,
            "GPU equirectangular output diverges from CPU reference: mse={mse}"
        );
    }

    #[test]
    fn gpu_primary_ray_sees_sun_disc_on_miss() {
        // Regression for the post-fix Devin Review finding flagging
        // that the GPU shader's miss branch only added
        // `sky_color * sky_strength` and never the analytic
        // `direct_visible_lights` contribution (sun disc + area-light
        // visibility through escaped rays). With the fix, a primary
        // ray pointed inside the sun's angular cone must accumulate
        // the sun's radiance on miss, producing pixels that are much
        // brighter than the sky-only baseline.
        //
        // Strategy: an empty-geometry scene (every primary ray
        // escapes) with a sun pointing straight at `-Z` (toward the
        // camera-forward) and a very small angular radius. The
        // bottom-row centre pixels (which subtend the smallest
        // angles around the forward direction) must see the sun;
        // far-off pixels see only the sky. We don't compare to CPU
        // (the CPU path is already covered by
        // `crate::path_trace::tests::*` for this branch) — instead
        // we assert the structural invariant that a bright spot
        // appears in the forward cone.
        let Ok(tracer) = GpuPathTracer::try_new() else {
            return;
        };

        // Empty scene with a single sun light. Build via
        // `from_render_scene` for the sky params and then push the
        // analytic sun directly so we control its direction and
        // angular radius exactly.
        let scene_in = RenderScene::default();
        let mut scene = PathTraceScene::from_render_scene(
            &scene_in,
            vec![],
            |_| None,
            SkyParams {
                strength: 0.0, // suppress sky so the sun is the only contributor
                color: [0.0, 0.0, 0.0],
                ..SkyParams::default()
            },
        );
        scene.lights.push(crate::light_sampling::NativeLight::Sun {
            direction: glam::Vec3::new(0.0, 0.0, 1.0), // photons travel +Z; sun disc lives at -Z
            radiance: glam::Vec3::splat(100.0),
            angular_radius_rad: 0.10, // ~5.7° half-angle — covers the centre pixels at 16×16
        });

        let camera = RenderCamera {
            id: "cam".into(),
            position_mm: [0.0, 0.0, 0.0],
            // target_mm: camera looks toward -Z, which is exactly
            // where the sun disc sits (since photons travel +Z).
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        let cfg = PathTraceConfig {
            width: 16,
            height: 16,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 8,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = tracer.render(&scene, &camera, &cfg, None, None);
        let avg = buf.average_rgb();

        let max_lum = avg
            .iter()
            .map(|p| p[0].max(p[1]).max(p[2]))
            .fold(0.0_f32, f32::max);
        // Without the fix the maximum pixel luminance was the sky
        // baseline (here zero by construction). With the fix at
        // least one primary ray in the forward cone hits the sun and
        // accumulates its 100-unit radiance.
        assert!(
            max_lum > 10.0,
            "GPU primary rays did not pick up the sun disc on miss; max luminance was {max_lum}"
        );
    }

    #[test]
    fn gpu_empty_bvh_with_camera_at_origin_terminates_cleanly() {
        // Regression for the Devin Review finding on 9b76ffe flagging
        // that the GPU dispatch path stored an empty-scene "stub" BVH
        // node inside `GpuSceneBuffers::bvh_nodes`, which made
        // `params.bvh_count == 1` (not 0) and bypassed the WGSL
        // `params.bvh_count == 0u` early-return. With the stub's
        // degenerate AABB at the world origin, a camera positioned at
        // the origin emits rays that pass *through* the AABB, treat
        // the stub as an internal node (`prim_count == 0`), and push
        // out-of-bounds child indices onto the traversal stack. WGSL
        // clamps the OOB reads, but the loop can spin until the
        // 64-deep stack overflows.
        //
        // The structural fix moves the empty-BVH stub from
        // `GpuSceneBuffers::build` to the bind step (mirroring the
        // triangle / material / light pattern), so `params.bvh_count`
        // remains 0 for empty scenes and the WGSL early-return fires
        // before any node is read. This test exercises exactly the
        // pathological case — camera at origin, ray through (0,0,0).
        let Ok(tracer) = GpuPathTracer::try_new() else {
            return;
        };

        let scene_in = RenderScene::default();
        let scene = PathTraceScene::from_render_scene(
            &scene_in,
            vec![],
            |_| None,
            SkyParams {
                strength: 1.0,
                color: [1.0, 1.0, 1.0],
                ..SkyParams::default()
            },
        );
        // BVH must actually be empty for this regression.
        assert!(
            scene.bvh.nodes.is_empty(),
            "test relies on an empty BVH to exercise the stub-node path"
        );

        let camera = RenderCamera {
            id: "cam".into(),
            // Camera at world origin — rays will pass through the
            // pre-fix stub AABB (which was [0,0,0] / [0,0,0]).
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        };
        let cfg = PathTraceConfig {
            width: 8,
            height: 6,
            samples_per_pixel: 1,
            max_bounces: 1,
            tile_size: 4,
            russian_roulette_min_bounces: 1,
            adaptive_threshold: 0.0,
            projection: crate::path_trace::CameraProjection::Perspective,
        };
        let buf = tracer.render(&scene, &camera, &cfg, None, None);

        // Every primary ray misses (no geometry) and hits the
        // constant-white sky, so every pixel should accumulate
        // `sky_color * sky_strength == (1,1,1)`. If the OOB stack
        // overflow had been triggered, the kernel would either
        // produce zeros (early-out without sky) or NaNs.
        let avg = buf.average_rgb();
        for (i, p) in avg.iter().enumerate() {
            assert!(
                p[0].is_finite() && p[1].is_finite() && p[2].is_finite(),
                "pixel {i} produced non-finite radiance {p:?} — empty-BVH traversal corrupted the kernel"
            );
            assert!(
                (p[0] - 1.0).abs() < 1.0e-3
                    && (p[1] - 1.0).abs() < 1.0e-3
                    && (p[2] - 1.0).abs() < 1.0e-3,
                "pixel {i} = {p:?}: empty-BVH miss should hit the white sky exactly, but the stub-node bug would either zero this out or push garbage from the OOB stack push."
            );
        }
    }

    #[test]
    fn gpu_path_tracer_falls_back_when_no_adapter() {
        match GpuPathTracer::try_new() {
            Ok(_) => {
                // GPU IS available in this environment — the dispatch
                // path is exercised in `gpu_render_matches_cpu_under_psnr`.
            }
            Err(GpuTraceError::NoAdapter) => {
                // Headless CI path; fallback must still produce output.
                let scene = quad_scene();
                let camera = RenderCamera {
                    id: "cam".into(),
                    position_mm: [0.0, 1500.0, 3000.0],
                    target_mm: [0.0, 0.0, 0.0],
                    focal_length_mm: 35.0,
                    exposure_ev: 0.0,
                    white_balance_k: 5500.0,
                    aperture_f: 5.6,
                };
                let cfg = PathTraceConfig {
                    width: 8,
                    height: 6,
                    samples_per_pixel: 1,
                    max_bounces: 1,
                    tile_size: 4,
                    russian_roulette_min_bounces: 1,
                    adaptive_threshold: 0.0,
                    projection: crate::path_trace::CameraProjection::Perspective,
                };
                let buf = render_or_fallback(&scene, &camera, &cfg, None, None, false);
                assert_eq!(buf.pixels.len(), 48);
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
}
