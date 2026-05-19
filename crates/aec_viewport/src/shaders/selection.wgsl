// Selection outline. Draws the silhouette of selected meshes as a glowing
// halo by rendering the geometry twice — once enlarged, once at original
// size with stencil masking.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_outline(vertex: VertexInput) -> VertexOutput {
    let pushed = vertex.position + vertex.normal * 25.0;
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4<f32>(pushed, 1.0);
    return out;
}

@fragment
fn fs_outline() -> @location(0) vec4<f32> {
    return vec4<f32>(0.486, 0.227, 0.929, 0.85);
}
