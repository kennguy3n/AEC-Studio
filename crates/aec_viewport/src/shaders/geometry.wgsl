// Geometry pass: position + normal + uv → world-space lit fragment.

struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

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
    @location(7) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) tint:         vec4<f32>,
};

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(instance.m0, instance.m1, instance.m2, instance.m3);
    let world_pos = model * vec4<f32>(vertex.position, 1.0);
    var out: VertexOutput;
    out.clip_position = camera.view_proj * world_pos;
    out.world_normal  = (model * vec4<f32>(vertex.normal, 0.0)).xyz;
    out.tint          = instance.tint;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let light_dir = normalize(vec3<f32>(0.5, 1.0, 0.5));
    let ndotl = max(dot(n, light_dir), 0.0);
    let ambient = vec3<f32>(0.18, 0.18, 0.20);
    let lit = ambient + ndotl * in.tint.rgb;
    return vec4<f32>(lit, in.tint.a);
}
