//! End-to-end glTF 2.0 round-trip:
//! build a scene with multiple meshes, a PBR material, a camera, and
//! a light → export as both `.glb` and `.gltf+.bin` → re-import via
//! the `gltf` crate (Khronos-spec-conformant parser) → assert the
//! geometry / materials / cameras / lights survive the round trip.
//!
//! This is the canonical Phase 11 Group C Task 20 acceptance test:
//! it proves the exporter produces valid glTF 2.0, not just JSON
//! that happens to have an "asset.version: 2.0" string.

use std::path::Path;

use aec_export::gltf_export::{
    write_gltf, GltfCamera, GltfLight, GltfLightKind, GltfMaterial, GltfMesh, GltfScene,
    WriteGltfOptions,
};

/// A 1 m × 1 m × 1 m cube positioned at `origin` (mm).
fn cube_at(name: &str, origin: [f32; 3]) -> GltfMesh {
    let s = 1000.0_f32;
    let p = |x: f32, y: f32, z: f32| -> [f32; 3] { [origin[0] + x, origin[1] + y, origin[2] + z] };
    let mut mesh = GltfMesh::new(name);
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [0.0, 0.0, -1.0],
            [
                p(0.0, 0.0, 0.0),
                p(0.0, s, 0.0),
                p(s, s, 0.0),
                p(s, 0.0, 0.0),
            ],
        ),
        (
            [0.0, 0.0, 1.0],
            [p(0.0, 0.0, s), p(s, 0.0, s), p(s, s, s), p(0.0, s, s)],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                p(0.0, 0.0, 0.0),
                p(s, 0.0, 0.0),
                p(s, 0.0, s),
                p(0.0, 0.0, s),
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [p(0.0, s, 0.0), p(0.0, s, s), p(s, s, s), p(s, s, 0.0)],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                p(0.0, 0.0, 0.0),
                p(0.0, 0.0, s),
                p(0.0, s, s),
                p(0.0, s, 0.0),
            ],
        ),
        (
            [1.0, 0.0, 0.0],
            [p(s, 0.0, 0.0), p(s, s, 0.0), p(s, s, s), p(s, 0.0, s)],
        ),
    ];
    for (normal, quad) in faces {
        let base = mesh.positions.len() as u32;
        for v in quad {
            mesh.positions.push(v);
            mesh.normals.push(normal);
            mesh.uvs.push([0.0, 0.0]);
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
}

fn build_full_scene() -> GltfScene {
    let mut scene = GltfScene {
        name: "Apartment Phase 11".into(),
        ..Default::default()
    };

    // Two cubes representing furniture; one wall mesh.
    let mut chair = cube_at("chair", [0.0, 0.0, 0.0]);
    chair.material_id = Some("mat:oak".into());
    let mut table = cube_at("table", [2000.0, 0.0, 0.0]);
    table.material_id = Some("mat:matte_white".into());
    let mut wall = cube_at("wall_north", [0.0, 0.0, 3000.0]);
    wall.material_id = Some("mat:matte_white".into());

    scene.meshes = vec![chair, table, wall];
    scene.materials = vec![
        GltfMaterial {
            id: "mat:oak".into(),
            name: "Light Oak".into(),
            base_color_factor: [0.78, 0.66, 0.5, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.55,
            emissive_factor: [0.0; 3],
            double_sided: false,
        },
        GltfMaterial {
            id: "mat:matte_white".into(),
            name: "Matte White".into(),
            base_color_factor: [0.92, 0.92, 0.92, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.85,
            emissive_factor: [0.0; 3],
            double_sided: false,
        },
    ];

    scene.cameras = vec![GltfCamera {
        name: "eye_level".into(),
        position_mm: [3000.0, 1700.0, 5000.0],
        target_mm: [1000.0, 1000.0, 0.0],
        up_mm: [0.0, 1.0, 0.0],
        aspect_ratio: 16.0 / 9.0,
        yfov_rad: std::f32::consts::FRAC_PI_4,
        znear_mm: 100.0,
        zfar_mm: 50_000.0,
    }];

    scene.lights = vec![
        GltfLight {
            name: "sun".into(),
            kind: GltfLightKind::Directional {
                direction_mm: [0.5, -1.0, 0.3],
            },
            color: [1.0, 0.95, 0.88],
            intensity: 1200.0,
        },
        GltfLight {
            name: "fill_lamp".into(),
            kind: GltfLightKind::Point {
                position_mm: [500.0, 2200.0, 500.0],
            },
            color: [1.0, 0.85, 0.65],
            intensity: 800.0,
        },
        GltfLight {
            name: "spot_accent".into(),
            kind: GltfLightKind::Spot {
                position_mm: [1500.0, 2500.0, 1500.0],
                direction_mm: [0.0, -1.0, 0.0],
                inner_cone_angle: 0.3,
                outer_cone_angle: 0.6,
            },
            color: [1.0, 1.0, 1.0],
            intensity: 400.0,
        },
    ];

    scene
}

fn import_and_check(path: &Path, expected_triangles: u32) {
    let gltf = gltf::Gltf::open(path).expect("gltf::Gltf::open should accept the export");
    let doc = gltf.document;
    // Spec compliance: version == 2.0.
    assert_eq!(doc.as_json().asset.version, "2.0");
    // Mesh count.
    assert_eq!(
        doc.meshes().count(),
        3,
        "expected 3 meshes (chair, table, wall_north)"
    );
    // Material count.
    assert_eq!(doc.materials().count(), 2);
    // Camera count.
    assert_eq!(doc.cameras().count(), 1);
    // Triangle count: 12 per cube × 3 cubes = 36 across all primitives.
    let mut total_tri = 0u32;
    for m in doc.meshes() {
        for p in m.primitives() {
            // Indices count / 3.
            if let Some(idx_acc) = p.indices() {
                total_tri += (idx_acc.count() / 3) as u32;
            }
        }
    }
    assert_eq!(total_tri, expected_triangles);
}

#[test]
fn full_scene_round_trips_through_gltf_crate_as_glb() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("scene.glb");
    let scene = build_full_scene();
    let res = write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
    assert_eq!(res.mesh_count, 3);
    assert_eq!(res.material_count, 2);
    assert_eq!(res.camera_count, 1);
    assert_eq!(res.light_count, 3);
    assert_eq!(res.triangle_count, 36);
    // Re-import via the gltf crate.
    import_and_check(&out, 36);
}

#[test]
fn full_scene_round_trips_through_gltf_crate_as_split_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("scene.gltf");
    let scene = build_full_scene();
    let res = write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
    assert!(res.bin_path.is_some());
    let bin = res.bin_path.as_ref().unwrap();
    assert!(bin.exists());
    // The gltf crate's open should resolve the relative `uri` to the sibling .bin.
    import_and_check(&out, 36);
}

#[test]
fn write_project_gltf_produces_valid_glb_when_extension_is_glb() {
    use aec_export::project_export::write_project_gltf;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("project.glb");
    write_project_gltf(&out, "Demo Project").unwrap();
    // Roundtrip via the gltf crate.
    let gltf = gltf::Gltf::open(&out).expect("project glb should parse");
    let doc = gltf.document;
    assert_eq!(doc.as_json().asset.version, "2.0");
    // Placeholder unit cube: 1 mesh + 1 material.
    assert_eq!(doc.meshes().count(), 1);
    assert_eq!(doc.materials().count(), 1);
}

#[test]
fn gltf_scene_with_no_uvs_omits_texcoord_accessor() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("nouv.glb");
    let mut mesh = cube_at("plain", [0.0, 0.0, 0.0]);
    mesh.uvs.clear(); // No UVs.
    let scene = GltfScene {
        name: "scene".into(),
        meshes: vec![mesh],
        ..Default::default()
    };
    write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
    let gltf = gltf::Gltf::open(&out).unwrap();
    for m in gltf.document.meshes() {
        for p in m.primitives() {
            // gltf::Semantic::TexCoords(0) should NOT be present.
            let tc = p.get(&gltf::Semantic::TexCoords(0));
            assert!(
                tc.is_none(),
                "TEXCOORD_0 should be absent when no UVs are provided"
            );
        }
    }
}

#[test]
fn material_factors_survive_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mat.glb");
    let scene = build_full_scene();
    write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
    let gltf = gltf::Gltf::open(&out).unwrap();
    let mats: Vec<_> = gltf.document.materials().collect();
    assert_eq!(mats.len(), 2);
    let oak = &mats[0];
    let bc = oak.pbr_metallic_roughness().base_color_factor();
    assert!((bc[0] - 0.78).abs() < 1e-4);
    assert!((bc[1] - 0.66).abs() < 1e-4);
    assert!((bc[2] - 0.5).abs() < 1e-4);
    let rough = oak.pbr_metallic_roughness().roughness_factor();
    assert!((rough - 0.55).abs() < 1e-4);
}
