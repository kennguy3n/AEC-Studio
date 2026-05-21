// Hardware picking pass. Renders the BIM tree using per-instance
// PickingIds and writes the id into a single R32Uint colour target.
// The host CPU resolves the cursor pixel to a PickingTarget through
// `aec_viewport::picking::PickRegistry`.
//
// 0 is reserved for "no hit" — host clears the target to 0 before each
// frame so any pixel that wasn't drawn comes back as the no-hit sentinel.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
};

struct InstanceInput {
    @location(1) m0:         vec4<f32>,
    @location(2) m1:         vec4<f32>,
    @location(3) m2:         vec4<f32>,
    @location(4) m3:         vec4<f32>,
    @location(5) picking_id: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) picking_id: u32,
};

@vertex
fn vs_main(v: VertexInput, i: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(i.m0, i.m1, i.m2, i.m3);
    var out: VertexOutput;
    out.clip_position = camera.view_proj * model * vec4<f32>(v.position, 1.0);
    out.picking_id = i.picking_id;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) u32 {
    return in.picking_id;
}
