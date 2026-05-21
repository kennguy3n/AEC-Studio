// PBR forward rasterizer with cascaded shadow maps.
//
// This is the navigable-viewport variant of `pbr.wgsl`. The two
// shaders share BRDF, IBL, and tone-mapping helpers verbatim; the only
// difference is the shadow path:
//
//   * `pbr.wgsl` (preview/offscreen): one shadow map, single matrix.
//   * `pbr_csm.wgsl` (this file): up to MAX_CASCADES shadow maps,
//     selected per-fragment by world-space view distance.
//
// The cascade count is supplied at runtime via `csm.cascade_count`
// so the host pipeline can degrade gracefully to 2 cascades on
// constrained GPUs without recompiling the shader.

const MAX_CASCADES: u32 = 4u;
const PI: f32 = 3.14159265358979323846;

struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

struct SunLight {
    direction: vec4<f32>,
    colour: vec4<f32>,
};

struct CsmCascade {
    light_view_proj: mat4x4<f32>,
    // [near_distance, far_distance, texel_size, _]
    range_texel: vec4<f32>,
};

struct CsmStack {
    // active count <= MAX_CASCADES; remaining slots are zeroed.
    cascade_count: u32,
    // [shadow_resolution_px, soft_shadow_kernel, depth_bias, normal_offset]
    params: vec4<f32>,
    _pad: vec3<u32>,
    cascades: array<CsmCascade, MAX_CASCADES>,
};

struct PbrSky {
    sun_dir_strength: vec4<f32>,
    coeff_y_abcd: vec4<f32>,
    coeff_x_abcd: vec4<f32>,
    coeff_yc_abcd: vec4<f32>,
    es_and_zenith_lum: vec4<f32>,
    tint_and_zenith: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera:      Camera;
@group(0) @binding(1) var<uniform> sun:         SunLight;
@group(0) @binding(2) var<uniform> csm:         CsmStack;
@group(0) @binding(3) var<uniform> sky:         PbrSky;
@group(0) @binding(4) var          shadow_maps: texture_depth_2d_array;
@group(0) @binding(5) var          shadow_samp: sampler_comparison;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
    @location(2) uv:       vec2<f32>,
};

struct InstanceInput {
    @location(3) m0:        vec4<f32>,
    @location(4) m1:        vec4<f32>,
    @location(5) m2:        vec4<f32>,
    @location(6) m3:        vec4<f32>,
    @location(7) base_pmr:  vec4<f32>,
    @location(8) rough_aoe: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos:    vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) base_color:   vec3<f32>,
    @location(3) metallic_roughness: vec2<f32>,
    @location(4) ao_emissive:  vec2<f32>,
    @location(5) view_depth:   f32,
};

@vertex
fn vs_main(v: VertexInput, i: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(i.m0, i.m1, i.m2, i.m3);
    let world_h = model * vec4<f32>(v.position, 1.0);
    let world_pos = world_h.xyz;
    let world_normal = normalize((model * vec4<f32>(v.normal, 0.0)).xyz);

    var out: VertexOutput;
    out.clip_position = camera.view_proj * world_h;
    out.world_pos = world_pos;
    out.world_normal = world_normal;
    out.base_color = i.base_pmr.xyz;
    out.metallic_roughness = vec2<f32>(i.base_pmr.w, i.rough_aoe.x);
    out.ao_emissive = vec2<f32>(i.rough_aoe.y, i.rough_aoe.z);
    // View-space depth = distance to camera, used to pick the cascade.
    out.view_depth = length(camera.camera_pos.xyz - world_pos);
    return out;
}

// --------------------------- BRDF helpers ----------------------------

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    let c = clamp(1.0 - cos_theta, 0.0, 1.0);
    return f0 + (vec3<f32>(1.0) - f0) * pow(c, 5.0);
}

fn ndf_ggx(ndh: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let denom = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / max(PI * denom * denom, 1.0e-6);
}

fn g_smith(ndv: f32, ndl: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let gv = ndv + sqrt(ndv * ndv * (1.0 - a2) + a2);
    let gl = ndl + sqrt(ndl * ndl * (1.0 - a2) + a2);
    return 1.0 / max(gv * gl, 1.0e-6);
}

// ----------------------------- Sky lookup ----------------------------
// (identical to pbr.wgsl — shared sky model for parity with the
//  preview pipeline)

fn preetham_f(coeff: vec4<f32>, e: f32, cos_theta: f32, cos_gamma: f32) -> f32 {
    let safe_cos_theta = max(cos_theta, 1.0e-3);
    let gamma = acos(clamp(cos_gamma, -1.0, 1.0));
    return (1.0 + coeff.x * exp(coeff.y / safe_cos_theta))
         * (1.0 + coeff.z * exp(coeff.w * gamma) + e * cos_gamma * cos_gamma);
}

fn xyy_to_rgb(big_y: f32, x: f32, y_chrom: f32) -> vec3<f32> {
    let safe_y = max(y_chrom, 1.0e-6);
    let cap_x = x * big_y / safe_y;
    let cap_y = big_y;
    let cap_z = (1.0 - x - y_chrom) * big_y / safe_y;
    let r =  3.2404542 * cap_x - 1.5371385 * cap_y - 0.4985314 * cap_z;
    let g = -0.9692660 * cap_x + 1.8760108 * cap_y + 0.0415560 * cap_z;
    let b =  0.0556434 * cap_x - 0.2040259 * cap_y + 1.0572252 * cap_z;
    return max(vec3<f32>(r, g, b), vec3<f32>(0.0));
}

fn sample_sky(direction: vec3<f32>) -> vec3<f32> {
    let dir = normalize(direction);
    let sun_dir = normalize(sky.sun_dir_strength.xyz);
    let tint = sky.tint_and_zenith.xyz;
    let strength = sky.sun_dir_strength.w;

    if (dir.y <= 0.0) {
        return tint * 0.05 * strength;
    }
    let cos_theta = max(dir.y, 1.0e-4);
    let cos_gamma = clamp(dot(dir, sun_dir), -1.0, 1.0);
    let sun_zenith = acos(clamp(sun_dir.y, -1.0, 1.0));
    let cos_sun_zenith = max(cos(sun_zenith), 1.0e-4);

    let zenith_pack = sky.tint_and_zenith.w;
    let zx = floor(zenith_pack) / 100.0;
    let zy = zenith_pack - floor(zenith_pack);

    let e_y = sky.es_and_zenith_lum.x;
    let e_x = sky.es_and_zenith_lum.y;
    let e_yc = sky.es_and_zenith_lum.z;
    let zy_lum = max(sky.es_and_zenith_lum.w, 0.0);

    let f_y  = preetham_f(sky.coeff_y_abcd,  e_y,  cos_theta, cos_gamma);
    let f_x  = preetham_f(sky.coeff_x_abcd,  e_x,  cos_theta, cos_gamma);
    let f_yc = preetham_f(sky.coeff_yc_abcd, e_yc, cos_theta, cos_gamma);
    let f0_y  = preetham_f(sky.coeff_y_abcd,  e_y,  1.0, cos_sun_zenith);
    let f0_x  = preetham_f(sky.coeff_x_abcd,  e_x,  1.0, cos_sun_zenith);
    let f0_yc = preetham_f(sky.coeff_yc_abcd, e_yc, 1.0, cos_sun_zenith);

    let big_y = max(zy_lum * f_y / f0_y, 0.0);
    let chroma_x = zx * f_x / f0_x;
    let chroma_y = zy * f_yc / f0_yc;
    let rgb = xyy_to_rgb(big_y, chroma_x, chroma_y);
    return rgb * tint * strength * (1.0 / 15.0);
}

// --------------------------- Shadow sampling -------------------------

// Pick a cascade by world-space view distance. The closest cascade
// whose `far_distance` is greater than the fragment depth wins. We
// iterate explicitly (not via array indexing) so the loop unrolls on
// the typical 3-cascade case.
fn select_cascade(view_depth: f32) -> u32 {
    let n = csm.cascade_count;
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        if (view_depth <= csm.cascades[i].range_texel.y) {
            return i;
        }
    }
    // Past all cascades — return the last one and let the bounds
    // check inside `sample_shadow_cascade` fall back to "fully lit".
    return n;
}

fn sample_shadow_cascade(world_pos: vec3<f32>, world_n: vec3<f32>, cascade: u32, ndl: f32) -> f32 {
    if (cascade >= csm.cascade_count) {
        return 1.0;
    }
    let c = csm.cascades[cascade];
    // Normal-offset shift to reduce acne on grazing-angle surfaces.
    let offset = world_n * csm.params.w * (1.0 - ndl);
    let pos_h = c.light_view_proj * vec4<f32>(world_pos + offset, 1.0);
    if (pos_h.w <= 0.0) {
        return 1.0;
    }
    let ndc = pos_h.xyz / pos_h.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 1.0;
    }
    let bias = max(csm.params.z * (1.0 - ndl), csm.params.z * 0.1);
    let depth = ndc.z - bias;
    // 3x3 PCF — fixed kernel for now. PCSS-style filter-size growth
    // is a follow-up (the CPU pipeline already exposes
    // `CsmParams::soft_shadows` for it).
    let texel = c.range_texel.z;
    var sum: f32 = 0.0;
    for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
        for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompare(
                shadow_maps, shadow_samp, uv + off, i32(cascade), depth
            );
        }
    }
    return sum / 9.0;
}

// ----------------------------- Tone map ------------------------------

fn tonemap_pbr_neutral(in: vec3<f32>) -> vec3<f32> {
    let start_compression = 0.8 - 0.04;
    let desaturation = 0.15;
    let x = min(in.r, min(in.g, in.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    var col = in - vec3<f32>(offset);
    let peak = max(col.r, max(col.g, col.b));
    if (peak < start_compression) {
        return col;
    }
    let d = 1.0 - start_compression;
    let new_peak = 1.0 - d * d / (peak + d - start_compression);
    col = col * (new_peak / peak);
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(col, vec3<f32>(new_peak), g);
}

// ----------------------------- Fragment ------------------------------

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let v = normalize(camera.camera_pos.xyz - in.world_pos);
    let l = normalize(sun.direction.xyz);
    let h = normalize(v + l);

    let base_color = in.base_color;
    let metallic = clamp(in.metallic_roughness.x, 0.0, 1.0);
    let roughness = clamp(in.metallic_roughness.y, 0.04, 1.0);
    let ao = clamp(in.ao_emissive.x, 0.0, 1.0);
    let emissive_strength = max(in.ao_emissive.y, 0.0);

    let ndl = max(dot(n, l), 0.0);
    let ndv = max(dot(n, v), 0.0);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);

    let alpha = roughness * roughness;
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let f = fresnel_schlick(vdh, f0);
    let d = ndf_ggx(ndh, alpha);
    let g = g_smith(ndv, ndl, alpha);
    let specular = d * g * f;

    let diffuse = (vec3<f32>(1.0) - f) * (1.0 - metallic) * base_color / PI;

    let cascade = select_cascade(in.view_depth);
    let shadow = sample_shadow_cascade(in.world_pos, n, cascade, ndl);
    let direct = (diffuse + specular) * sun.colour.rgb * ndl * shadow;

    let ibl_diffuse = sample_sky(n) * (1.0 - metallic) * base_color * ao;
    let r = reflect(-v, n);
    let ibl_spec_raw = sample_sky(r);
    let ibl_specular = mix(ibl_spec_raw, ibl_diffuse, roughness) * f0;

    let emissive = base_color * emissive_strength;
    let radiance = direct + ibl_diffuse + ibl_specular + emissive;

    let mapped = tonemap_pbr_neutral(radiance);
    return vec4<f32>(mapped, 1.0);
}
