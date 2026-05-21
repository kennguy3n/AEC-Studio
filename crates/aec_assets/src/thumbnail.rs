//! Asset thumbnail rendering.
//!
//! Renders a small PBR preview of an asset mesh using the existing CPU
//! path tracer in [`aec_render::path_trace`]. We deliberately use the
//! CPU path so thumbnails are reproducible in headless CI and on
//! machines without a GPU. The output is a PNG-encoded byte buffer that
//! the asset DB stores as a content-addressed blob.
//!
//! Frame composition: the mesh is auto-framed so its bounding sphere
//! fills ~75% of the rendered image, viewed from a 3/4 isometric angle
//! (azimuth 35°, elevation 25°). Lighting is a neutral overcast sky;
//! the asset's own material library is used when supplied.

use aec_geometry::Mesh;
use aec_render::lighting::SkyParams;
use aec_render::material::PathTraceMaterial;
use aec_render::path_trace::{
    render, AccumulationBuffer, CameraProjection, PathTraceConfig, PathTraceScene,
};
use aec_render::scene::{RenderCamera, RenderScene, SerializedMesh};
use glam::Vec3;

/// Errors returned by thumbnail rendering.
#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("mesh is empty (no positions or no indices)")]
    EmptyMesh,
    #[error("thumbnail dimensions must be > 0 and ≤ 4096; got {0}×{1}")]
    InvalidSize(u32, u32),
    #[error("png encode: {0}")]
    Encode(String),
}

/// Thumbnail rendering options. All fields have sensible defaults — most
/// callers should just use [`ThumbnailOptions::default`].
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailOptions {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Samples per pixel. Default 32 — a balance between speed and
    /// quality for ~256×256 thumbnails.
    pub samples_per_pixel: u32,
    /// Max ray bounces. Default 3 — good enough for diffuse furniture
    /// without spending samples on caustics.
    pub max_bounces: u32,
    /// Camera azimuth in radians (yaw around the mesh's centre).
    /// Default 35° -> π/180 × 35.
    pub camera_azimuth_rad: f32,
    /// Camera elevation in radians (pitch above the equator).
    /// Default 25°.
    pub camera_elevation_rad: f32,
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self {
            width: 256,
            height: 256,
            samples_per_pixel: 32,
            max_bounces: 3,
            camera_azimuth_rad: 35.0_f32.to_radians(),
            camera_elevation_rad: 25.0_f32.to_radians(),
        }
    }
}

/// Render a thumbnail of `mesh` to a PNG byte buffer.
pub fn render_thumbnail(mesh: &Mesh, opts: &ThumbnailOptions) -> Result<Vec<u8>, ThumbnailError> {
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(ThumbnailError::EmptyMesh);
    }
    if opts.width == 0 || opts.height == 0 || opts.width > 4096 || opts.height > 4096 {
        return Err(ThumbnailError::InvalidSize(opts.width, opts.height));
    }

    let scene = build_path_trace_scene(mesh);
    let (camera, _bounds) = auto_frame_camera(
        mesh,
        opts.width as f32,
        opts.height as f32,
        opts.camera_azimuth_rad,
        opts.camera_elevation_rad,
    );
    let config = PathTraceConfig {
        width: opts.width,
        height: opts.height,
        samples_per_pixel: opts.samples_per_pixel.max(1),
        max_bounces: opts.max_bounces.max(1),
        tile_size: 32,
        russian_roulette_min_bounces: 2,
        adaptive_threshold: 0.0,
        projection: CameraProjection::Perspective,
    };

    let buf: AccumulationBuffer = render(&scene, &camera, &config, None, None);
    let srgb = buf.as_srgb8();
    encode_png(&srgb, opts.width, opts.height)
}

/// Build a single-mesh [`PathTraceScene`] using the default neutral
/// grey material and an overcast sky. Mesh world transform is the
/// identity — we re-frame the camera instead.
fn build_path_trace_scene(mesh: &Mesh) -> PathTraceScene {
    let sm = SerializedMesh {
        id: "asset".to_string(),
        indices: mesh.indices.clone(),
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        uvs: mesh.uvs.clone(),
        material_id: None,
        transform: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let render_scene = RenderScene {
        meshes: vec![sm],
        cameras: Vec::new(),
        lights: vec![aec_render::scene::RenderLight::SunSky {
            azimuth_deg: 30.0,
            elevation_deg: 60.0,
            intensity: 1.2,
            color_temperature_k: 6500.0,
        }],
        ambient_strength: 0.3,
    };
    PathTraceScene::from_render_scene(
        &render_scene,
        vec![PathTraceMaterial::default_grey()],
        |_| None,
        SkyParams::default(),
    )
}

/// Compute the mesh bounding box, build a camera that frames the mesh
/// inside the image, and return both. The camera is placed at
/// (azimuth, elevation) on a sphere with radius proportional to the
/// bounding-sphere radius so the projected silhouette fills ~75% of the
/// image.
fn auto_frame_camera(
    mesh: &Mesh,
    width: f32,
    height: f32,
    azimuth: f32,
    elevation: f32,
) -> (RenderCamera, ([f32; 3], [f32; 3])) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in &mesh.positions {
        for i in 0..3 {
            if p[i] < min[i] {
                min[i] = p[i];
            }
            if p[i] > max[i] {
                max[i] = p[i];
            }
        }
    }
    let centre = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let extent = Vec3::new(max[0] - min[0], max[1] - min[1], max[2] - min[2]);
    let radius = (extent.length() * 0.5).max(1.0);

    // 50 mm focal length on a 36 mm full-frame sensor -> ~40° vertical FOV.
    // The path tracer ignores aspect explicitly (it derives it from
    // width/height), so we just pick a comfortable working distance.
    let aspect = (width / height).max(0.001);
    // Distance such that the bounding sphere fills ~75% of the image
    // when projected through a 40° vertical FOV.
    let fov_y = 40.0_f32.to_radians();
    let dist_vertical = radius / (fov_y * 0.5).tan();
    let dist_horizontal = radius / ((fov_y * 0.5).tan() * aspect);
    let distance = dist_vertical.max(dist_horizontal) / 0.75;

    let ca = azimuth.cos();
    let sa = azimuth.sin();
    let ce = elevation.cos();
    let se = elevation.sin();
    let dir = Vec3::new(ca * ce, se, sa * ce); // camera-from-target direction
    let position = Vec3::new(centre[0], centre[1], centre[2]) + dir * distance;

    let camera = RenderCamera {
        id: "asset-thumb".to_string(),
        position_mm: position.into(),
        target_mm: centre,
        focal_length_mm: 50.0,
        exposure_ev: 0.0,
        white_balance_k: 6500.0,
        aperture_f: 8.0,
    };
    (camera, (min, max))
}

fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, ThumbnailError> {
    let img = image::RgbImage::from_raw(width, height, rgb.to_vec()).ok_or_else(|| {
        ThumbnailError::Encode(format!(
            "rgb buffer length {} does not match {}×{}×3",
            rgb.len(),
            width,
            height
        ))
    })?;
    let mut out = Vec::with_capacity(rgb.len());
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| ThumbnailError::Encode(format!("png: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube() -> Mesh {
        let mut m = Mesh::new();
        // 6 quads, axis-aligned cube centred on origin, 1mm side.
        let faces: [([f32; 3], [f32; 3], [f32; 3], [f32; 3], [f32; 3]); 6] = [
            // +X
            (
                [0.5, -0.5, -0.5],
                [0.5, 0.5, -0.5],
                [0.5, 0.5, 0.5],
                [0.5, -0.5, 0.5],
                [1.0, 0.0, 0.0],
            ),
            // -X
            (
                [-0.5, -0.5, 0.5],
                [-0.5, 0.5, 0.5],
                [-0.5, 0.5, -0.5],
                [-0.5, -0.5, -0.5],
                [-1.0, 0.0, 0.0],
            ),
            // +Y
            (
                [-0.5, 0.5, -0.5],
                [-0.5, 0.5, 0.5],
                [0.5, 0.5, 0.5],
                [0.5, 0.5, -0.5],
                [0.0, 1.0, 0.0],
            ),
            // -Y
            (
                [-0.5, -0.5, 0.5],
                [-0.5, -0.5, -0.5],
                [0.5, -0.5, -0.5],
                [0.5, -0.5, 0.5],
                [0.0, -1.0, 0.0],
            ),
            // +Z
            (
                [-0.5, -0.5, 0.5],
                [0.5, -0.5, 0.5],
                [0.5, 0.5, 0.5],
                [-0.5, 0.5, 0.5],
                [0.0, 0.0, 1.0],
            ),
            // -Z
            (
                [0.5, -0.5, -0.5],
                [-0.5, -0.5, -0.5],
                [-0.5, 0.5, -0.5],
                [0.5, 0.5, -0.5],
                [0.0, 0.0, -1.0],
            ),
        ];
        for (a, b, c, d, n) in faces {
            m.push_quad(
                a,
                b,
                c,
                d,
                n,
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
        }
        m
    }

    #[test]
    fn render_thumbnail_returns_valid_png_header() {
        let mesh = unit_cube();
        let opts = ThumbnailOptions {
            width: 64,
            height: 64,
            samples_per_pixel: 1,
            max_bounces: 1,
            ..Default::default()
        };
        let png = render_thumbnail(&mesh, &opts).expect("render");
        // PNG magic: 0x89 0x50 0x4E 0x47 0x0D 0x0A 0x1A 0x0A
        assert_eq!(&png[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    }

    #[test]
    fn empty_mesh_rejected() {
        let mesh = Mesh::new();
        let err = render_thumbnail(&mesh, &ThumbnailOptions::default()).unwrap_err();
        assert!(matches!(err, ThumbnailError::EmptyMesh));
    }

    #[test]
    fn zero_dimensions_rejected() {
        let mesh = unit_cube();
        let opts = ThumbnailOptions {
            width: 0,
            height: 64,
            ..Default::default()
        };
        let err = render_thumbnail(&mesh, &opts).unwrap_err();
        assert!(matches!(err, ThumbnailError::InvalidSize(0, 64)));
    }

    #[test]
    fn oversized_dimensions_rejected() {
        let mesh = unit_cube();
        let opts = ThumbnailOptions {
            width: 8192,
            height: 64,
            ..Default::default()
        };
        let err = render_thumbnail(&mesh, &opts).unwrap_err();
        assert!(matches!(err, ThumbnailError::InvalidSize(8192, 64)));
    }

    #[test]
    fn camera_frames_mesh_in_view() {
        let mesh = unit_cube();
        let opts = ThumbnailOptions::default();
        let (_cam, (min, max)) =
            auto_frame_camera(&mesh, opts.width as f32, opts.height as f32, 0.0, 0.0);
        // Bounding box should be the unit cube extent.
        for i in 0..3 {
            assert!((min[i] - (-0.5)).abs() < 1e-6);
            assert!((max[i] - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn camera_distance_scales_with_mesh_size() {
        let small = {
            let mut m = Mesh::new();
            m.push_quad(
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
            m
        };
        let large = {
            let mut m = Mesh::new();
            m.push_quad(
                [0.0, 0.0, 0.0],
                [100.0, 0.0, 0.0],
                [100.0, 100.0, 0.0],
                [0.0, 100.0, 0.0],
                [0.0, 0.0, 1.0],
                [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            );
            m
        };
        let (cam_small, _) = auto_frame_camera(&small, 256.0, 256.0, 0.0, 0.0);
        let (cam_large, _) = auto_frame_camera(&large, 256.0, 256.0, 0.0, 0.0);
        let d_small =
            Vec3::from_array(cam_small.position_mm) - Vec3::from_array(cam_small.target_mm);
        let d_large =
            Vec3::from_array(cam_large.position_mm) - Vec3::from_array(cam_large.target_mm);
        assert!(d_large.length() > d_small.length() * 10.0);
    }
}
