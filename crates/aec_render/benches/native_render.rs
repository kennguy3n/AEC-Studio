//! Native render-pipeline latency benchmark.
//!
//! Phase 9 removed the Blender worker; the equivalent latency
//! measurement now lives inside the Rust crate as a single-tile path
//! trace call. Three benchmarks:
//!
//! * `path_trace_single_tile_perspective` — 64×64 perspective render at
//!   one sample per pixel. This is the closest analogue to the previous
//!   EEVEE preview latency benchmark.
//! * `path_trace_single_tile_panorama` — same scene, equirectangular
//!   projection. Guards against regressions in the panorama ray-gen.
//! * `scene_compile` — measures how long it takes to convert a
//!   [`RenderScene`] into a [`PathTraceScene`] (BVH build + light
//!   conversion). Picked because the legacy benchmark also measured
//!   serialisation cost; the new analogue is the BVH/material compile
//!   step, since there is no more IPC payload.
//!
//! Run with: `cargo bench -p aec_render`.

use std::hint::black_box;

use aec_materials::MaterialLibrary;
use aec_render::{
    final_render::FinalRenderPipeline,
    path_trace::{render_tile_pass, CameraProjection, PathTraceConfig, PathTraceScene, Tile},
    RenderCamera, RenderLight, RenderPreset, RenderScene, SerializedMesh, SkyParams,
};
use criterion::{criterion_group, criterion_main, Criterion};

/// Build a representative scene: ~10 k triangles split across 20
/// meshes, one camera, one sun. Picked to mirror the apartment
/// template benchmark target from PROGRESS.md.
fn make_scene() -> RenderScene {
    let mut scene = RenderScene::new();

    for mesh_idx in 0..20 {
        let mut positions = Vec::with_capacity(1500);
        let mut normals = Vec::with_capacity(1500);
        let mut uvs = Vec::with_capacity(1500);
        let mut indices = Vec::with_capacity(1500);
        for tri in 0..500u32 {
            let base = (mesh_idx * 500 + tri) as f32;
            for v in 0..3 {
                let f = base + v as f32 * 0.1;
                positions.push([f.cos() * 1000.0, f.sin() * 1000.0, (f * 0.5).cos() * 1000.0]);
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
            material_id: None,
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        });
    }

    scene.push_camera(RenderCamera {
        id: "cam0".into(),
        position_mm: [0.0, 1500.0, 4000.0],
        target_mm: [0.0, 0.0, 0.0],
        focal_length_mm: 35.0,
        exposure_ev: 0.0,
        white_balance_k: 5500.0,
        aperture_f: 5.6,
    });
    scene.push_light(RenderLight::SunSky {
        azimuth_deg: 135.0,
        elevation_deg: 45.0,
        intensity: 2.0,
        color_temperature_k: 5500.0,
    });
    scene
}

fn compile_scene(scene: &RenderScene) -> PathTraceScene {
    let materials = MaterialLibrary::new();
    // Mirror the conversion logic the final-render pipeline uses.
    let entries: Vec<(String, aec_render::material::PathTraceMaterial)> = materials
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                aec_render::material::PathTraceMaterial::from_pbr(m),
            )
        })
        .collect();
    let path_trace_materials: Vec<aec_render::material::PathTraceMaterial> =
        entries.iter().map(|(_, m)| *m).collect();
    PathTraceScene::from_render_scene(
        scene,
        path_trace_materials,
        |id| entries.iter().position(|(eid, _)| eid == id),
        SkyParams::default(),
    )
}

fn bench_scene_compile(c: &mut Criterion) {
    let scene = make_scene();
    c.bench_function("scene_compile", |b| {
        b.iter(|| {
            let pt = compile_scene(black_box(&scene));
            black_box(pt);
        });
    });
}

fn bench_single_tile_perspective(c: &mut Criterion) {
    let scene = make_scene();
    let camera = scene.cameras[0].clone();
    let pt = compile_scene(&scene);
    let config = PathTraceConfig {
        width: 64,
        height: 64,
        samples_per_pixel: 1,
        max_bounces: 3,
        tile_size: 64,
        russian_roulette_min_bounces: 2,
        adaptive_threshold: 0.0,
        projection: CameraProjection::Perspective,
    };
    let tile = Tile {
        x_start: 0,
        y_start: 0,
        x_end: 64,
        y_end: 64,
    };
    c.bench_function("path_trace_single_tile_perspective", |b| {
        b.iter(|| {
            let r = render_tile_pass(black_box(&pt), &camera, &config, tile, 1, 0, 0xDEAD_BEEF);
            black_box(r);
        });
    });
}

fn bench_single_tile_panorama(c: &mut Criterion) {
    let scene = make_scene();
    let camera = scene.cameras[0].clone();
    let pt = compile_scene(&scene);
    let config = PathTraceConfig {
        width: 64,
        height: 32,
        samples_per_pixel: 1,
        max_bounces: 3,
        tile_size: 64,
        russian_roulette_min_bounces: 2,
        adaptive_threshold: 0.0,
        projection: CameraProjection::Equirectangular,
    };
    let tile = Tile {
        x_start: 0,
        y_start: 0,
        x_end: 64,
        y_end: 32,
    };
    c.bench_function("path_trace_single_tile_panorama", |b| {
        b.iter(|| {
            let r = render_tile_pass(black_box(&pt), &camera, &config, tile, 1, 0, 0xDEAD_BEEF);
            black_box(r);
        });
    });
}

fn bench_final_render_pipeline_e2e(c: &mut Criterion) {
    // End-to-end (compile + render + write) at a tiny resolution so the
    // benchmark stays fast on CI while still exercising every layer.
    let scene = make_scene();
    let pipeline = FinalRenderPipeline::new();
    let mut preset = RenderPreset::quick();
    preset.config.resolution_x = 32;
    preset.config.resolution_y = 24;
    preset.config.samples = 1;
    preset.config.tile_size_px = 32;
    preset.config.denoise = false;
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("e2e.png");
    c.bench_function("final_render_pipeline_e2e", |b| {
        b.iter(|| {
            let r = pipeline.render(black_box(&scene), &preset, &out).unwrap();
            black_box(r);
        });
    });
}

criterion_group!(
    benches,
    bench_scene_compile,
    bench_single_tile_perspective,
    bench_single_tile_panorama,
    bench_final_render_pipeline_e2e
);
criterion_main!(benches);
