// Hi-Z (hierarchical Z) depth pyramid builder + per-instance
// occlusion test.
//
// Two entry points:
//   * `build_mip`  — fullscreen pass that reads a mip level of the
//     scene depth target and emits the next coarser mip level as the
//     MAX of the 2x2 source footprint. We use MAX, not MIN, because
//     wgpu's reversed-Z depth convention puts the far plane at depth
//     0 and the near plane at depth 1; a fragment is occluded if its
//     conservative max-depth in NDC is less than every pixel of the
//     mip's footprint (i.e. it's behind everything already drawn).
//
//   * `occlusion_test` — fullscreen pass that consumes per-instance
//     screen-space rects + min-NDC-z (built by
//     `aec_viewport::culling::project_for_hiz`) and writes an opacity
//     flag into an R32Uint visibility target. The host reads it back
//     to mark instances as drawable for the next frame.
//
// Hi-Z trades 1 frame of latency for cheap GPU occlusion — the kind of
// tradeoff the host pipeline orchestrates so the shader stays
// portable.

struct OcclusionInstance {
    // [min_u, min_v, max_u, max_v] in [0,1] screen-space.
    rect: vec4<f32>,
    // [min_z, mip_level, straddles_near, _]
    z_mip: vec4<f32>,
};

@group(0) @binding(0) var          src_depth:   texture_2d<f32>;
@group(0) @binding(1) var          src_sampler: sampler;
@group(0) @binding(2) var<storage, read> instances: array<OcclusionInstance>;
@group(0) @binding(3) var<storage, read_write> visibility: array<u32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0)       uv:            vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VertexOutput;
    out.clip_position = vec4<f32>(uv * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

// Compute the MAX of a 2x2 footprint at the requested mip level.
// Used to author mip N+1 from mip N.
@fragment
fn fs_build_mip(in: VertexOutput) -> @location(0) f32 {
    let dims = textureDimensions(src_depth);
    let coord = vec2<i32>(
        i32(in.uv.x * f32(dims.x)),
        i32(in.uv.y * f32(dims.y)),
    );
    let a = textureLoad(src_depth, coord,                       0).r;
    let b = textureLoad(src_depth, coord + vec2<i32>(1, 0),     0).r;
    let c = textureLoad(src_depth, coord + vec2<i32>(0, 1),     0).r;
    let d = textureLoad(src_depth, coord + vec2<i32>(1, 1),     0).r;
    return max(max(a, b), max(c, d));
}

// Per-instance occlusion test. One thread per instance — dispatched
// with a compute shader rather than a pixel shader because we want
// indexed-write semantics (one instance updates one visibility slot).
@compute @workgroup_size(64)
fn cs_occlusion_test(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&instances)) {
        return;
    }
    let inst = instances[i];

    // Instances whose AABB straddles the near plane are kept visible
    // unconditionally: their projected rect is unreliable and we'd
    // rather draw an unnecessary triangle than miss something the
    // camera is inside.
    if (inst.z_mip.z > 0.5) {
        visibility[i] = 1u;
        return;
    }

    let mip = u32(inst.z_mip.y);
    let dims = textureDimensions(src_depth, mip);
    // Sample the corners of the rect at the chosen mip; the worst
    // (deepest) of the four is the conservative depth of the
    // strip we care about. If the instance's min-NDC-z is in front
    // of that depth (i.e., min_z > footprint_max in reversed-Z), the
    // instance is potentially visible.
    let u0 = inst.rect.x; let v0 = inst.rect.y;
    let u1 = inst.rect.z; let v1 = inst.rect.w;
    let p00 = vec2<i32>(i32(u0 * f32(dims.x)), i32(v0 * f32(dims.y)));
    let p10 = vec2<i32>(i32(u1 * f32(dims.x)), i32(v0 * f32(dims.y)));
    let p01 = vec2<i32>(i32(u0 * f32(dims.x)), i32(v1 * f32(dims.y)));
    let p11 = vec2<i32>(i32(u1 * f32(dims.x)), i32(v1 * f32(dims.y)));
    let d00 = textureLoad(src_depth, p00, i32(mip)).r;
    let d10 = textureLoad(src_depth, p10, i32(mip)).r;
    let d01 = textureLoad(src_depth, p01, i32(mip)).r;
    let d11 = textureLoad(src_depth, p11, i32(mip)).r;
    let footprint_max = max(max(d00, d10), max(d01, d11));

    // Reversed-Z: visible if instance's nearest depth is *greater*
    // than the footprint's deepest depth (it's in front of all
    // existing geometry).
    if (inst.z_mip.x >= footprint_max) {
        visibility[i] = 1u;
    } else {
        visibility[i] = 0u;
    }
}
