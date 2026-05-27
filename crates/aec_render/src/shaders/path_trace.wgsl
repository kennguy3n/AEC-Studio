// GPU compute shader path tracer — mirrors the CPU megakernel in
// `crate::path_trace::trace_path`. Single-bounce direct lighting plus
// stack-based BVH traversal. Fits on every wgpu adapter (no atomics
// required on the accumulation buffer; the host dispatches tiles
// serially).
//
// Bindings (all in group 0):
//   binding 0: read-only storage buffer of BvhNodeGpu
//   binding 1: read-only storage buffer of TriangleGpu
//   binding 2: read-only storage buffer of MaterialGpu
//   binding 3: read-only storage buffer of LightGpu
//   binding 4: uniform `Params`
//   binding 5: read-write storage buffer of `vec4<f32>` accumulators
//
// The host produces input buffers via `gpu_trace::build_scene_buffers`
// and reads back the accumulator via standard wgpu buffer copy.

struct BvhNodeGpu {
    min: vec3<f32>,
    left_or_start: u32,
    max: vec3<f32>,
    prim_count: u32,
}

struct TriangleGpu {
    v0: vec3<f32>,
    pad0: f32,
    v1: vec3<f32>,
    pad1: f32,
    v2: vec3<f32>,
    material_id: i32,
}

struct MaterialGpu {
    base_color: vec3<f32>,
    metallic: f32,
    emissive: vec3<f32>,
    roughness: f32,
    f0: vec3<f32>,
    ior: f32,
}

struct LightGpu {
    // kind:
    //   0 = sun (direction in `position`, radius in `params.x`,
    //       color * intensity in `emission`)
    //   1 = point (position, color * intensity in `emission`)
    //   2 = area (position, u-axis in `params.xyz`, v-axis in `extra`,
    //       size in `size`, emission)
    kind: u32,
    position: vec3<f32>,
    params: vec4<f32>,
    extra: vec4<f32>,
    size: vec2<f32>,
    emission: vec3<f32>,
    pad: f32,
}

struct Params {
    width: u32,
    height: u32,
    samples_per_pixel: u32,
    max_bounces: u32,
    tri_count: u32,
    bvh_count: u32,
    mat_count: u32,
    light_count: u32,
    camera_origin: vec3<f32>,
    focal_half_h: f32,
    camera_right: vec3<f32>,
    aspect: f32,
    camera_up: vec3<f32>,
    pad_up: f32,
    camera_forward: vec3<f32>,
    seed: u32,
    sky_color: vec3<f32>,
    sky_strength: f32,
    // Camera projection: 0 = perspective, 1 = equirectangular.
    // Must mirror `crate::path_trace::CameraProjection` discriminants.
    // WGSL auto-pads the struct end to the max-field alignment (16
    // bytes here, set by `vec3<f32>`), so the on-device size is 128
    // bytes — matches `ParamsGpu` in `gpu_trace.rs` (also 128).
    projection: u32,
}

@group(0) @binding(0) var<storage, read> bvh_nodes: array<BvhNodeGpu>;
@group(0) @binding(1) var<storage, read> triangles: array<TriangleGpu>;
@group(0) @binding(2) var<storage, read> materials: array<MaterialGpu>;
@group(0) @binding(3) var<storage, read> lights: array<LightGpu>;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var<storage, read_write> accum: array<vec4<f32>>;
// Phase 12 Task 23: IES candela atlas. Per-light slice is
// IES_ATLAS_W * IES_ATLAS_H floats; the slice offset is packed in
// `light.params.x` for kind=3u (IES) lights. The shader samples
// candela via bilinear interpolation on the (vertical, horizontal)
// direction angles.
@group(0) @binding(6) var<storage, read> ies_atlas: array<f32>;

const IES_ATLAS_W: u32 = 64u;
const IES_ATLAS_H: u32 = 32u;
const IES_ATLAS_SLICE: u32 = 2048u; // IES_ATLAS_W * IES_ATLAS_H

// Sample the IES candela atlas at direction `dir` (world-space, from
// light to receiver). `forward` and `up` define the luminaire frame.
// Returns interpolated candela value (cd) for the given direction.
fn sample_ies_atlas(slice_offset: u32, dir: vec3<f32>, forward: vec3<f32>, up: vec3<f32>) -> f32 {
    // Transform direction into luminaire local frame. The IES vertical
    // angle is measured from the downward axis (the "nadir"), which is
    // -forward in our convention. Horizontal angle is measured around
    // forward, with 0 at +up.
    let fwd = normalize(forward);
    let upn = normalize(up - fwd * dot(up, fwd));
    let right = normalize(cross(fwd, upn));
    let local_x = dot(dir, right);
    let local_y = dot(dir, upn);
    let local_z = dot(dir, fwd);
    // Vertical: angle from downward (-fwd) axis. cos(theta_v) = -local_z.
    let cos_v = clamp(-local_z, -1.0, 1.0);
    let theta_v = acos(cos_v); // [0, pi]
    let v_frac = theta_v / 3.14159265359; // [0, 1]
    // Horizontal: atan2(x, y) gives angle around the forward axis, with
    // 0 at +y (up). Wrap to [0, 1].
    var phi_h = atan2(local_x, local_y); // [-pi, pi]
    if (phi_h < 0.0) {
        phi_h = phi_h + 6.2831853;
    }
    let h_frac = phi_h / 6.2831853; // [0, 1)
    // Bilinear sample
    let fx = h_frac * f32(IES_ATLAS_W - 1u);
    let fy = v_frac * f32(IES_ATLAS_H - 1u);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, IES_ATLAS_W - 1u);
    let y1 = min(y0 + 1u, IES_ATLAS_H - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let base = slice_offset * IES_ATLAS_SLICE;
    let c00 = ies_atlas[base + y0 * IES_ATLAS_W + x0];
    let c10 = ies_atlas[base + y0 * IES_ATLAS_W + x1];
    let c01 = ies_atlas[base + y1 * IES_ATLAS_W + x0];
    let c11 = ies_atlas[base + y1 * IES_ATLAS_W + x1];
    let c0 = c00 + (c10 - c00) * tx;
    let c1 = c01 + (c11 - c01) * tx;
    return c0 + (c1 - c0) * ty;
}

// Tiny PCG-style RNG so every workgroup invocation has a uncorrelated
// sequence even for the small SPP we dispatch from compute.
fn pcg_hash(seed: u32) -> u32 {
    var state = seed * 747796405u + 2891336453u;
    state = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (state >> 22u) ^ state;
}

fn rand_f32(state: ptr<function, u32>) -> f32 {
    *state = pcg_hash(*state);
    return f32(*state) * (1.0 / 4294967296.0);
}

struct Hit {
    t: f32,
    u: f32,
    v: f32,
    prim_id: u32,
    valid: u32,
}

fn ray_tri(ray_origin: vec3<f32>, ray_dir: vec3<f32>, t_max: f32, prim_id: u32) -> Hit {
    let tri = triangles[prim_id];
    let edge1 = tri.v1 - tri.v0;
    let edge2 = tri.v2 - tri.v0;
    let h = cross(ray_dir, edge2);
    let a = dot(edge1, h);
    if (abs(a) < 1.0e-7) {
        return Hit(0.0, 0.0, 0.0, prim_id, 0u);
    }
    let f = 1.0 / a;
    let s = ray_origin - tri.v0;
    let u = f * dot(s, h);
    if (u < 0.0 || u > 1.0) {
        return Hit(0.0, 0.0, 0.0, prim_id, 0u);
    }
    let q = cross(s, edge1);
    let v = f * dot(ray_dir, q);
    if (v < 0.0 || u + v > 1.0) {
        return Hit(0.0, 0.0, 0.0, prim_id, 0u);
    }
    let t = f * dot(edge2, q);
    if (t > 1.0e-4 && t < t_max) {
        return Hit(t, u, v, prim_id, 1u);
    }
    return Hit(0.0, 0.0, 0.0, prim_id, 0u);
}

fn ray_aabb_t(ray_origin: vec3<f32>, inv_dir: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, t_max: f32) -> vec2<f32> {
    let t1 = (lo - ray_origin) * inv_dir;
    let t2 = (hi - ray_origin) * inv_dir;
    let tmin = min(t1, t2);
    let tmax = max(t1, t2);
    let near = max(max(tmin.x, tmin.y), tmin.z);
    let far = min(min(tmax.x, tmax.y), tmax.z);
    if (far < 0.0 || near > far || near > t_max) {
        return vec2<f32>(-1.0, -1.0);
    }
    return vec2<f32>(max(near, 0.0), far);
}

fn traverse(ray_origin: vec3<f32>, ray_dir: vec3<f32>, t_max_in: f32) -> Hit {
    var best = Hit(t_max_in, 0.0, 0.0, 0u, 0u);
    if (params.bvh_count == 0u) {
        return best;
    }
    // Sign-preserving safe inverse, matching the CPU `safe_inverse` in
    // `intersect.rs`. A finite, signed magnitude is used in place of +/-inf
    // so that the subsequent multiplication `(lo - origin) * inv_dir` never
    // produces NaN when the origin lies exactly on a slab face. Preserving
    // the sign keeps the slab test correct regardless of `t_max` magnitude.
    let safe_x = select(1.0 / ray_dir.x, sign(ray_dir.x) * 1.0e30, abs(ray_dir.x) < 1.0e-12);
    let safe_y = select(1.0 / ray_dir.y, sign(ray_dir.y) * 1.0e30, abs(ray_dir.y) < 1.0e-12);
    let safe_z = select(1.0 / ray_dir.z, sign(ray_dir.z) * 1.0e30, abs(ray_dir.z) < 1.0e-12);
    // `sign(0)` is 0 in WGSL — guard so an exactly-zero component still
    // produces a finite positive magnitude rather than 0.
    let inv_dir = vec3<f32>(
        select(safe_x, 1.0e30, safe_x == 0.0),
        select(safe_y, 1.0e30, safe_y == 0.0),
        select(safe_z, 1.0e30, safe_z == 0.0),
    );
    var stack: array<u32, 64>;
    var sp: i32 = 0;
    stack[0] = 0u;
    sp = 1;
    while (sp > 0) {
        sp = sp - 1;
        let node_idx = stack[sp];
        let node = bvh_nodes[node_idx];
        let hit_aabb = ray_aabb_t(ray_origin, inv_dir, node.min, node.max, best.t);
        if (hit_aabb.x < 0.0) {
            continue;
        }
        if (node.prim_count == 0u) {
            // Internal — push both children in arbitrary order.
            if (sp + 2 <= 64) {
                stack[sp] = node.left_or_start;
                sp = sp + 1;
                stack[sp] = node.left_or_start + 1u;
                sp = sp + 1;
            }
        } else {
            let start = node.left_or_start;
            for (var i: u32 = 0u; i < node.prim_count; i = i + 1u) {
                let h = ray_tri(ray_origin, ray_dir, best.t, start + i);
                if (h.valid == 1u) {
                    best = h;
                }
            }
        }
    }
    return best;
}

fn shadow_ray(ray_origin: vec3<f32>, ray_dir: vec3<f32>, t_max: f32) -> bool {
    let h = traverse(ray_origin, ray_dir, t_max);
    return h.valid == 1u;
}

fn triangle_normal(prim_id: u32) -> vec3<f32> {
    let tri = triangles[prim_id];
    return normalize(cross(tri.v1 - tri.v0, tri.v2 - tri.v0));
}

fn material_for(prim_id: u32) -> MaterialGpu {
    let tri = triangles[prim_id];
    let mat_id = tri.material_id;
    if (mat_id < 0 || u32(mat_id) >= params.mat_count) {
        return MaterialGpu(
            vec3<f32>(0.5),
            0.0,
            vec3<f32>(0.0),
            0.5,
            vec3<f32>(0.04),
            1.5,
        );
    }
    return materials[u32(mat_id)];
}

fn sample_light_gpu(light: LightGpu, hit_pos: vec3<f32>, r0: f32, r1: f32, out_dir: ptr<function, vec3<f32>>, out_dist: ptr<function, f32>, out_emitted: ptr<function, vec3<f32>>, out_pdf: ptr<function, f32>) -> bool {
    if (light.kind == 1u) {
        // Point.
        let d = light.position - hit_pos;
        let dist2 = max(dot(d, d), 1.0e-6);
        let dist = sqrt(dist2);
        *out_dir = d / dist;
        *out_dist = dist;
        *out_emitted = light.emission / dist2;
        *out_pdf = 1.0;
        return true;
    } else if (light.kind == 0u) {
        // Sun.
        *out_dir = normalize(-light.position);
        *out_dist = 1.0e6;
        *out_emitted = light.emission;
        *out_pdf = 1.0;
        return true;
    } else if (light.kind == 2u) {
        // Area.
        let u_axis = light.params.xyz;
        let v_axis = light.extra.xyz;
        let center = light.position;
        let sample_pos = center + u_axis * (r0 - 0.5) * light.size.x + v_axis * (r1 - 0.5) * light.size.y;
        let d = sample_pos - hit_pos;
        let dist2 = max(dot(d, d), 1.0e-6);
        let dist = sqrt(dist2);
        let dir = d / dist;
        let n_area = normalize(cross(u_axis, v_axis));
        let cos_at_light = max(0.0, dot(-dir, n_area));
        if (cos_at_light <= 0.0) {
            return false;
        }
        let area = light.size.x * light.size.y;
        *out_dir = dir;
        *out_dist = dist;
        *out_emitted = light.emission;
        *out_pdf = dist2 / (area * cos_at_light);
        return true;
    } else if (light.kind == 3u) {
        // Phase 12 Task 23: IES light. Sample the candela atlas at the
        // direction from light to hit point, scale emission by candela
        // * intensity_scale / r².
        let slice_offset = u32(light.params.x);
        let forward = light.params.yzw;
        let up = light.extra.xyz;
        let intensity_scale = light.extra.w;
        let d = hit_pos - light.position;
        let dist2 = max(dot(d, d), 1.0e-6);
        let dist = sqrt(dist2);
        let dir_l_to_r = d / dist;
        let candela = sample_ies_atlas(slice_offset, dir_l_to_r, forward, up);
        *out_dir = -dir_l_to_r;
        *out_dist = dist;
        *out_emitted = light.emission * (candela * intensity_scale / dist2);
        *out_pdf = 1.0;
        return true;
    }
    return false;
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    let c = clamp(1.0 - cos_theta, 0.0, 1.0);
    return f0 + (vec3<f32>(1.0) - f0) * (c * c * c * c * c);
}

fn tangent_basis(n: vec3<f32>, t: ptr<function, vec3<f32>>, b: ptr<function, vec3<f32>>) {
    let s = select(1.0, -1.0, n.z < 0.0);
    let a = -1.0 / (s + n.z);
    let bp = n.x * n.y * a;
    *t = vec3<f32>(1.0 + s * n.x * n.x * a, s * bp, -s * n.x);
    *b = vec3<f32>(bp, s + n.y * n.y * a, -n.y);
}

fn cosine_weighted_sample(n: vec3<f32>, r0: f32, r1: f32) -> vec3<f32> {
    let r = sqrt(r0);
    let phi = 6.2831853 * r1;
    let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - r0)));
    var t: vec3<f32>;
    var b: vec3<f32>;
    tangent_basis(n, &t, &b);
    return normalize(t * local.x + b * local.y + n * local.z);
}

// Analytic emission seen by a ray that misses all geometry — mirrors
// `crate::path_trace::direct_visible_lights`. Without this branch a
// primary ray pointed at the sun (or a panorama row covering the sun
// disc) would only pick up the diffuse sky background, while the CPU
// kernel would correctly accumulate the analytic light's radiance.
//
// Point / IES lights are intentionally excluded: they are delta
// emitters with zero solid angle, so a ray can never "hit" one — their
// illumination flows entirely through NEE.
//
// Caller must gate this to the GPU's analog of CPU's
// `last_was_specular` (currently `bounce == 0u`) so that emission
// already accounted for by NEE during the bouncing loop is not
// double-counted.
fn direct_visible_lights(ray_o: vec3<f32>, ray_d: vec3<f32>) -> vec3<f32> {
    var total = vec3<f32>(0.0);
    for (var li: u32 = 0u; li < params.light_count; li = li + 1u) {
        let light = lights[li];
        if (light.kind == 0u) {
            // Sun: `light.position` is the (normalized) direction the
            // photons travel toward the scene, so the apparent
            // direction of the sun disc is `-light.position`. The ray
            // "hits" the sun if its direction falls inside the disc's
            // angular cone.
            let to_sun = -light.position;
            let cos_cone = cos(light.params.x);
            let cos_angle = dot(normalize(ray_d), normalize(to_sun));
            if (cos_angle >= cos_cone) {
                total = total + light.emission;
            }
        } else if (light.kind == 2u) {
            // Area: ray-plane intersection, then a (u, v) rectangle
            // test in the light's local frame. The area light is
            // two-sided, matching the CPU implementation.
            let u_axis = light.params.xyz;
            let v_axis = light.extra.xyz;
            let n_area = normalize(cross(u_axis, v_axis));
            let denom = dot(n_area, ray_d);
            if (abs(denom) >= 1.0e-6) {
                let t = dot(light.position - ray_o, n_area) / denom;
                // Primary-ray bounds: `t_min = 1e-4`, `t_max = 1e8`.
                // CPU uses `[ray.t_min, ray.t_max]` (1e-4 to
                // `f32::INFINITY` for primary rays); the upper-bound
                // check is a defence-in-depth no-op today but keeps
                // CPU and GPU symbolically identical so a future
                // change that tightens `t_max` won't silently diverge
                // the two kernels. `1e8` matches the traversal cap
                // already passed to `traverse(..., 1e8)`.
                if (t > 1.0e-4 && t < 1.0e8) {
                    let hit_pt = ray_o + ray_d * t;
                    let local = hit_pt - light.position;
                    let u = dot(local, u_axis);
                    let v = dot(local, v_axis);
                    if (abs(u) <= light.size.x * 0.5 && abs(v) <= light.size.y * 0.5) {
                        total = total + light.emission;
                    }
                }
            }
        }
        // kind == 1u (point / IES): skip; delta emitters are not
        // hit-visible.
    }
    return total;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let pixel_idx = gid.y * params.width + gid.x;
    var rng_state: u32 = (gid.x * 1973u + gid.y * 9277u + params.seed * 26699u) | 1u;

    var accum_rgb = vec3<f32>(0.0);
    for (var s: u32 = 0u; s < params.samples_per_pixel; s = s + 1u) {
        let jx = rand_f32(&rng_state);
        let jy = rand_f32(&rng_state);
        var dir_view: vec3<f32>;
        if (params.projection == 1u) {
            // Equirectangular: centre column (u=0.5) maps to view
            // -Z (camera forward) via `phi = (u - 0.5) * TAU`,
            // matching the standard 360°/VR convention. Must mirror
            // `crate::path_trace::equirectangular_dir` exactly so CPU
            // and GPU produce identical panoramas. The cardinal
            // columns are: u=0/u=1 → +Z (back), u=0.25 → -X (left),
            // u=0.5 → -Z (forward), u=0.75 → +X (right).
            let u = (f32(gid.x) + jx) / max(f32(params.width), 1.0);
            let v = (f32(gid.y) + jy) / max(f32(params.height), 1.0);
            let phi = (u - 0.5) * 6.28318530717958647692;   // (u - 0.5) * TAU
            let theta = v * 3.14159265358979323846;          // v * PI
            let sin_theta = sin(theta);
            // `dir_view` is unit-length by construction
            // (`sin²θ (sin²φ + cos²φ) + cos²θ = 1`); the basis
            // multiplication below applies a `normalize(...)` to
            // absorb any floating-point drift, so we don't normalize
            // here.
            dir_view = vec3<f32>(sin_theta * sin(phi),
                                  cos(theta),
                                  -sin_theta * cos(phi));
        } else {
            // Perspective: pinhole projection through the camera
            // focal length / aspect ratio.
            let nx = (f32(gid.x) + jx) / f32(params.width) * 2.0 - 1.0;
            let ny = 1.0 - (f32(gid.y) + jy) / f32(params.height) * 2.0;
            dir_view = normalize(vec3<f32>(nx * params.focal_half_h * params.aspect,
                                            ny * params.focal_half_h,
                                            -1.0));
        }
        let dir_world = normalize(params.camera_right * dir_view.x
                                + params.camera_up * dir_view.y
                                - params.camera_forward * dir_view.z);
        var ray_o = params.camera_origin;
        var ray_d = dir_world;
        var throughput = vec3<f32>(1.0);
        var radiance = vec3<f32>(0.0);

        for (var bounce: u32 = 0u; bounce < params.max_bounces; bounce = bounce + 1u) {
            let hit = traverse(ray_o, ray_d, 1.0e8);
            if (hit.valid == 0u) {
                radiance = radiance + throughput * params.sky_color * params.sky_strength;
                // Cycles parity: a ray that escapes the scene also
                // sees any analytic light whose support contains the
                // ray direction. Gated to `bounce == 0u` — the GPU
                // megakernel uses cosine-weighted diffuse bounces for
                // every non-primary segment, which is the analog of
                // the CPU kernel's `last_was_specular = false` after
                // a non-specular sample. Direct NEE inside the loop
                // already accounts for analytic lights on every hit,
                // so adding the analytic miss contribution after a
                // diffuse bounce would double-count.
                if (bounce == 0u) {
                    radiance = radiance + throughput * direct_visible_lights(ray_o, ray_d);
                }
                break;
            }
            let mat = material_for(hit.prim_id);
            let n = triangle_normal(hit.prim_id);
            let nn = select(-n, n, dot(n, -ray_d) > 0.0);
            let hit_pos = ray_o + ray_d * hit.t + nn * 1.0e-4;

            // Direct emission only on first bounce (no MIS yet on the GPU
            // megakernel — the bilateral denoiser cleans it up).
            if (bounce == 0u) {
                radiance = radiance + throughput * mat.emissive;
            }

            // Single NEE for each light.
            for (var li: u32 = 0u; li < params.light_count; li = li + 1u) {
                let light = lights[li];
                var ldir: vec3<f32>;
                var ldist: f32;
                var emit: vec3<f32>;
                var lpdf: f32;
                if (sample_light_gpu(light, hit_pos, rand_f32(&rng_state),
                                     rand_f32(&rng_state), &ldir, &ldist, &emit, &lpdf)) {
                    let cos_at_surface = dot(nn, ldir);
                    if (cos_at_surface > 0.0 && lpdf > 0.0) {
                        if (!shadow_ray(hit_pos, ldir, ldist - 1.0e-3)) {
                            let f0 = mat.f0;
                            let fr = fresnel_schlick(max(cos_at_surface, 0.0), f0);
                            let diffuse = mat.base_color * (1.0 - mat.metallic) / 3.14159265;
                            let bsdf = diffuse * (vec3<f32>(1.0) - fr);
                            radiance = radiance + throughput * bsdf * emit * cos_at_surface / lpdf;
                        }
                    }
                }
            }

            // Cosine-weighted diffuse bounce (specular GPU lobe arrives
            // in a follow-up; the CPU kernel covers the high-quality
            // path).
            let new_dir = cosine_weighted_sample(nn, rand_f32(&rng_state), rand_f32(&rng_state));
            let cos_nd = max(dot(nn, new_dir), 0.0);
            if (cos_nd <= 0.0) {
                break;
            }
            // PDF for cosine-weighted = cos / pi, BSDF = base_color / pi.
            // → throughput multiplier = base_color * (1 - metallic).
            throughput = throughput * mat.base_color * (1.0 - mat.metallic);
            ray_o = hit_pos;
            ray_d = new_dir;

            // Russian roulette after bounce 3.
            if (bounce >= 3u) {
                let p_cont = clamp(max(throughput.x, max(throughput.y, throughput.z)), 0.05, 0.95);
                if (rand_f32(&rng_state) > p_cont) {
                    break;
                }
                throughput = throughput / p_cont;
            }
        }

        accum_rgb = accum_rgb + radiance;
    }

    let n = f32(params.samples_per_pixel);
    let avg = accum_rgb / max(n, 1.0);
    accum[pixel_idx] = vec4<f32>(avg, f32(params.samples_per_pixel));
}
