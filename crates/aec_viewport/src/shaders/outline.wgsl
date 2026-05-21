// Jump-flood selection outline composite.
//
// The CPU pass (`aec_viewport::outline::jump_flood_sdf`) has already
// produced a squared-distance field over the framebuffer; this shader
// reads it as an R32Uint texture and composes a fixed-width outline
// onto the colour target.
//
// We chose the JFA approach (over the alternative of an edge-detect
// kernel on the picking buffer) because:
//   1. it produces sub-pixel-stable outlines that don't shimmer as
//      objects move;
//   2. the thickness is exact, not an approximation tied to the
//      kernel radius;
//   3. it composes correctly with anti-aliasing already in the
//      colour buffer (we mix per-pixel rather than overwriting).
//
// The squared-distance field is computed CPU-side, so the only
// per-frame GPU cost is one fullscreen pass.

struct OutlineParams {
    // [thickness_px^2, fade_band_px^2, dimensions ignored, _]
    thickness2_fade: vec4<f32>,
    // RGBA outline colour, premultiplied by intended opacity.
    colour: vec4<f32>,
};

@group(0) @binding(0) var          sdf_tex:     texture_2d<u32>;
@group(0) @binding(1) var<uniform> params:      OutlineParams;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0)       uv:            vec2<f32>,
};

// Fullscreen triangle (3 vertices, no buffer binding). Vertex IDs
// 0/1/2 map to a triangle that covers the [-1,1]^2 NDC quad with the
// UV ranging over [0,1]^2.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VertexOutput;
    out.clip_position = vec4<f32>(uv * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dims = textureDimensions(sdf_tex);
    let pixel = vec2<i32>(
        i32(in.uv.x * f32(dims.x)),
        i32(in.uv.y * f32(dims.y)),
    );
    let d2 = f32(textureLoad(sdf_tex, pixel, 0).r);

    let t2   = params.thickness2_fade.x;
    let fade = params.thickness2_fade.y;
    // Inside the seed: discard (the renderer's main pass already
    // drew the selected object, we don't want to overdraw it).
    if (d2 == 0.0) {
        discard;
    }
    // Outside the outline band: discard so the underlying colour
    // buffer shows through unmodified.
    if (d2 > t2 + fade) {
        discard;
    }
    // Soft outer edge: linear ramp from full opacity at t^2 to zero
    // at t^2 + fade.
    let a = clamp(1.0 - max(d2 - t2, 0.0) / max(fade, 1.0e-6), 0.0, 1.0);
    return vec4<f32>(params.colour.rgb, params.colour.a * a);
}
