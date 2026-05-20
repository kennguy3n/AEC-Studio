//! EEVEE preview latency benchmark.
//!
//! Phase 5 promises a 250 ms EEVEE round-trip on a target machine.
//! The *real* end-to-end latency includes Blender's EEVEE renderer,
//! which we can't run inside `cargo bench`. Instead we measure the
//! Rust-side overhead of the IPC pipeline — scene construction,
//! request serialisation, and response deserialisation — and let the
//! Blender-side budget be measured manually on a real install.
//!
//! With this baseline we can catch performance regressions in the
//! serialisation layer alone (which historically has been the cause
//! of "EEVEE feels sluggish" complaints when scenes grow large).
//!
//! Run with: `cargo bench -p aec_render`.

use std::hint::black_box;

use aec_render::{
    BlenderRequest, RenderCamera, RenderLight, RenderPreset, RenderScene, SerializedMesh,
};
use criterion::{criterion_group, criterion_main, Criterion};

/// Build a representative scene: ~10 k triangles split across 20
/// meshes, four cameras, three lights. Picked to mirror the apartment
/// template benchmark target from PROGRESS.md.
fn make_scene() -> RenderScene {
    let mut scene = RenderScene::new();
    scene.ambient_strength = 0.15;

    // 20 meshes × 500 tris each.
    for mesh_idx in 0..20 {
        let mut positions = Vec::with_capacity(1500);
        let mut normals = Vec::with_capacity(1500);
        let mut uvs = Vec::with_capacity(1500);
        let mut indices = Vec::with_capacity(1500);
        for tri in 0..500u32 {
            let base = (mesh_idx * 500 + tri) as f32;
            for v in 0..3 {
                let f = base + v as f32 * 0.1;
                positions.push([f.cos(), f.sin(), (f * 0.5).cos()]);
                normals.push([0.0, 0.0, 1.0]);
                uvs.push([(f * 0.1).fract(), (f * 0.2).fract()]);
            }
            let i0 = tri * 3;
            indices.extend_from_slice(&[i0, i0 + 1, i0 + 2]);
        }
        scene.push_mesh(SerializedMesh {
            id: format!("mesh_{mesh_idx:02}"),
            indices,
            positions,
            normals,
            uvs,
            material_id: Some(format!("mat_{:02}", mesh_idx % 5)),
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        });
    }

    for cam_idx in 0..4 {
        scene.push_camera(RenderCamera {
            id: format!("camera_{cam_idx}"),
            position_mm: [cam_idx as f32 * 1000.0, 0.0, 1500.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        });
    }

    scene.push_light(RenderLight::SunSky {
        azimuth_deg: 135.0,
        elevation_deg: 45.0,
        intensity: 1.5,
        color_temperature_k: 5500.0,
    });
    scene.push_light(RenderLight::Area {
        position_mm: [500.0, 500.0, 2400.0],
        width_mm: 600.0,
        height_mm: 600.0,
        intensity: 400.0,
        color_temperature_k: 3200.0,
    });
    scene.push_light(RenderLight::Point {
        position_mm: [-500.0, 500.0, 2400.0],
        intensity: 80.0,
        color_temperature_k: 3000.0,
    });
    scene
}

fn bench_scene_construction(c: &mut Criterion) {
    c.bench_function("eevee_preview_scene_construction", |b| {
        b.iter(|| {
            let scene = make_scene();
            black_box(scene);
        });
    });
}

fn bench_request_serialise(c: &mut Criterion) {
    let scene = make_scene();
    let preset = RenderPreset::eevee_preview();
    c.bench_function("eevee_preview_request_serialise", |b| {
        b.iter(|| {
            let req = BlenderRequest::EeveeRender {
                scene: Box::new(scene.clone()),
                preset: Box::new(preset.clone()),
                output_path: "/tmp/eevee.png".into(),
            };
            let json = serde_json::to_string(&req).unwrap();
            black_box(json);
        });
    });
}

fn bench_response_deserialise(c: &mut Criterion) {
    // Mock RenderCompleted response — the worker writes this to stdout
    // when EEVEE finishes; we measure how long it takes us to parse.
    let payload = r#"{
        "type": "render_completed",
        "job_id": "job_42",
        "output_path": "/tmp/eevee.png"
    }"#;
    c.bench_function("eevee_preview_response_deserialise", |b| {
        b.iter(|| {
            let resp: aec_render::BlenderResponse = serde_json::from_str(payload).unwrap();
            black_box(resp);
        });
    });
}

criterion_group!(
    benches,
    bench_scene_construction,
    bench_request_serialise,
    bench_response_deserialise
);
criterion_main!(benches);
