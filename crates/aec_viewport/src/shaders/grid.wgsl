// Infinite ground grid. Drawn as a single full-screen triangle pair with
// world-space derivation in the fragment shader.

struct Camera {
    view_proj_inv: mat4x4<f32>,
    camera_pos:    vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ray_origin: vec3<f32>,
    @location(1) ray_dir:    vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = positions[vid];
    var out: VertexOutput;
    out.clip_position = vec4<f32>(p, 0.999, 1.0);
    let near = camera.view_proj_inv * vec4<f32>(p, -1.0, 1.0);
    let far  = camera.view_proj_inv * vec4<f32>(p,  1.0, 1.0);
    out.ray_origin = near.xyz / near.w;
    out.ray_dir    = normalize(far.xyz / far.w - out.ray_origin);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = -in.ray_origin.y / in.ray_dir.y;
    if (t < 0.0) { discard; }
    let xz = in.ray_origin.xz + in.ray_dir.xz * t;
    let minor = 100.0;
    let major = 1000.0;
    let minor_d = abs(fract(xz / minor) - 0.5);
    let major_d = abs(fract(xz / major) - 0.5);
    let minor_line = step(min(minor_d.x, minor_d.y), 0.02);
    let major_line = step(min(major_d.x, major_d.y), 0.005);
    let alpha = max(minor_line * 0.4, major_line * 0.7);
    return vec4<f32>(0.72, 0.72, 0.78, alpha);
}
