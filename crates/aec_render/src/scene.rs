//! Render scene serialization shared by the preview, final, walkthrough,
//! and panorama pipelines. Phase 9 PR4 removed the Blender worker; the
//! structures here are now consumed natively by
//! [`crate::path_trace::PathTraceScene::from_render_scene`] (CPU/GPU
//! path tracer) and [`crate::preview::PreviewPipeline`] (PBR raster).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SerializedMesh {
    pub id: String,
    /// Triangle indices packed [i0, i1, i2, i0, i1, i2, ...].
    pub indices: Vec<u32>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub material_id: Option<String>,
    /// World transform encoded as a 4x4 row-major matrix (mm).
    pub transform: [[f32; 4]; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderCamera {
    pub id: String,
    pub position_mm: [f32; 3],
    pub target_mm: [f32; 3],
    pub focal_length_mm: f32,
    pub exposure_ev: f32,
    pub white_balance_k: f32,
    pub aperture_f: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderLight {
    SunSky {
        azimuth_deg: f32,
        elevation_deg: f32,
        intensity: f32,
        color_temperature_k: f32,
    },
    Area {
        position_mm: [f32; 3],
        width_mm: f32,
        height_mm: f32,
        intensity: f32,
        color_temperature_k: f32,
    },
    Point {
        position_mm: [f32; 3],
        intensity: f32,
        color_temperature_k: f32,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderScene {
    pub meshes: Vec<SerializedMesh>,
    pub cameras: Vec<RenderCamera>,
    pub lights: Vec<RenderLight>,
    pub ambient_strength: f32,
}

impl RenderScene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_mesh(&mut self, mesh: SerializedMesh) {
        self.meshes.push(mesh);
    }

    pub fn push_camera(&mut self, camera: RenderCamera) {
        self.cameras.push(camera);
    }

    pub fn push_light(&mut self, light: RenderLight) {
        self.lights.push(light);
    }

    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len() / 3).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_count_sums_meshes() {
        let mut s = RenderScene::new();
        s.push_mesh(SerializedMesh {
            id: "m1".into(),
            indices: vec![0, 1, 2, 0, 1, 2],
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            material_id: None,
            transform: [[0.0; 4]; 4],
        });
        assert_eq!(s.triangle_count(), 2);
    }
}
