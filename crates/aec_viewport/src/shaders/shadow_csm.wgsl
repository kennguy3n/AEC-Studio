// Depth-only shadow caster for cascaded shadow maps.
//
// Drawn once per cascade with the cascade's light_view_proj. Writes
// only depth (no colour target) so it's bandwidth-cheap.
//
// The cascade selection on the lit pass happens in `pbr.wgsl` using
// the world-space distance from camera; this shader is just the
// depth render for whichever cascade is being authored.

struct ShadowCamera {
    light_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> shadow_camera: ShadowCamera;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

struct InstanceInput {
    @location(1) m0: vec4<f32>,
    @location(2) m1: vec4<f32>,
    @location(3) m2: vec4<f32>,
    @location(4) m3: vec4<f32>,
};

@vertex
fn vs_main(v: VertexInput, i: InstanceInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(i.m0, i.m1, i.m2, i.m3);
    return shadow_camera.light_view_proj * model * vec4<f32>(v.position, 1.0);
}

// No fragment shader: depth-only pass. wgpu picks up `vs_main`'s
// @builtin(position) and emits the implicit depth output.
