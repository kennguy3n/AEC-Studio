// Procedural sky shader (Preetham 1999). Mirrors the Rust reference
// implementation in `aec_viewport/src/sky.rs`. Rendered as a fullscreen
// triangle in front of all geometry by `pbr_preview.rs`; also bound by
// the path tracer's environment lookup when present.

struct SkyUniform {
    // sun_dir.xyz, strength in .w
    sun_dir_strength: vec4<f32>,
    // Preetham Y coefficients: A, B, C, D
    coeff_y_abcd: vec4<f32>,
    // Preetham x coefficients: A, B, C, D
    coeff_x_abcd: vec4<f32>,
    // Preetham y_chroma coefficients: A, B, C, D
    coeff_yc_abcd: vec4<f32>,
    // E_y, E_x, E_yc, zenith_luminance
    es_and_zenith_lum: vec4<f32>,
    // tint.rgb in xyz; zenith chromaticity pack (floor(zx*100) + zy) in .w
    tint_and_zenith: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sky: SkyUniform;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// Fullscreen-triangle vertex shader. Three vertices cover the screen
// without needing a vertex buffer. Depth = 1 (far plane) so the sky
// renders behind everything that wrote to z < 1.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = positions[vid];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(p, 1.0, 1.0);
    out.ndc = p;
    return out;
}

// Camera uniform — must mirror the Rust `CameraUniform` layout in
// `pbr_preview.rs` exactly (view_proj at offset 0, inv_view_proj at
// offset 64, camera_pos at offset 128). The sky pass binds the same
// `camera_buf` as the main PBR pass, so the struct layout below MUST
// match `CameraUniform` byte-for-byte.
struct CameraUniform {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

@group(0) @binding(1) var<uniform> camera: CameraUniform;

fn preetham_f(coeff: vec4<f32>, e: f32, cos_theta: f32, cos_gamma: f32) -> f32 {
    let a = coeff.x;
    let b = coeff.y;
    let c = coeff.z;
    let d = coeff.w;
    let safe_cos_theta = max(cos_theta, 1.0e-3);
    let gamma = acos(clamp(cos_gamma, -1.0, 1.0));
    return (1.0 + a * exp(b / safe_cos_theta))
         * (1.0 + c * exp(d * gamma) + e * cos_gamma * cos_gamma);
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

// Evaluate the Preetham sky in the supplied direction. `direction` is
// the normalised world-space view ray. Returns linear RGB scaled by tint
// and strength.
fn evaluate_sky(direction: vec3<f32>) -> vec3<f32> {
    let dir = normalize(direction);
    let sun = normalize(sky.sun_dir_strength.xyz);
    let tint = sky.tint_and_zenith.xyz;
    let strength = sky.sun_dir_strength.w;

    // Below horizon → dim ground colour. Matches the Rust reference.
    if (dir.y <= 0.0) {
        return tint * 0.05 * strength;
    }

    let cos_theta = max(dir.y, 1.0e-4);
    let cos_gamma = clamp(dot(dir, sun), -1.0, 1.0);

    // Reconstruct sun zenith from the sun direction. asin(sun.y) is
    // sun elevation; pi/2 minus that gives the zenith.
    let sun_zenith = acos(clamp(sun.y, -1.0, 1.0));
    let cos_sun_zenith = max(cos(sun_zenith), 1.0e-4);

    // Unpack zenith chromaticity from sky.tint_and_zenith.w.
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
    // Match the Rust reference: scale by 1/15 to put zenith into
    // [0,1] tone-mapping range, multiply by tint and strength.
    return rgb * tint * strength * (1.0 / 15.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // NDC → world-space ray direction. clip = (ndc.x, ndc.y, 1, 1),
    // world = inv_view_proj * clip; ray = normalize(world.xyz - camera).
    let clip = vec4<f32>(in.ndc, 1.0, 1.0);
    let world_h = camera.inv_view_proj * clip;
    let world = world_h.xyz / world_h.w;
    let dir = normalize(world - camera.camera_pos.xyz);
    let rgb = evaluate_sky(dir);
    return vec4<f32>(rgb, 1.0);
}
