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

impl SerializedMesh {
    /// Phase 12 Task 21: convert an `aec_geometry::Mesh` (the tessellated
    /// output of walls / floors / ceilings / furniture in the project
    /// graph) into a `SerializedMesh` suitable for the render scene.
    ///
    /// `id` and `material_id` are caller-supplied because the project
    /// graph entity carries this metadata separately from the raw mesh
    /// geometry; `transform` is the entity's world-space transform as a
    /// 4×4 row-major matrix.
    pub fn from_geometry_mesh(
        id: impl Into<String>,
        mesh: &aec_geometry::Mesh,
        material_id: Option<String>,
        transform: [[f32; 4]; 4],
    ) -> Self {
        // The geometry tessellator can emit meshes with empty UV arrays
        // (most procedural shapes don't need texture coordinates). The
        // path tracer assumes `uvs.len() == positions.len()`, so pad
        // with zeros when the source mesh has no UVs.
        let uvs = if mesh.uvs.len() == mesh.positions.len() {
            mesh.uvs.clone()
        } else {
            vec![[0.0_f32, 0.0]; mesh.positions.len()]
        };
        // Similarly, normals can be empty when only positions are
        // populated. Default to up so the renderer still gets a valid
        // basis instead of NaN-tangents.
        let normals = if mesh.normals.len() == mesh.positions.len() {
            mesh.normals.clone()
        } else {
            vec![[0.0_f32, 1.0, 0.0]; mesh.positions.len()]
        };
        SerializedMesh {
            id: id.into(),
            indices: mesh.indices.clone(),
            positions: mesh.positions.clone(),
            normals,
            uvs,
            material_id,
            transform,
        }
    }
}

impl RenderScene {
    /// Phase 12 Task 21: append a tessellated geometry mesh to the
    /// render scene with optional material binding and a 4×4 transform.
    pub fn push_geometry_mesh(
        &mut self,
        id: impl Into<String>,
        mesh: &aec_geometry::Mesh,
        material_id: Option<String>,
        transform: [[f32; 4]; 4],
    ) {
        self.meshes
            .push(SerializedMesh::from_geometry_mesh(
                id,
                mesh,
                material_id,
                transform,
            ));
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

    #[test]
    fn push_geometry_mesh_pads_missing_uvs_and_normals() {
        // Phase 12 Task 21: geometry meshes coming out of the wall /
        // floor tessellator can omit UVs or normals. The push helper
        // must pad them rather than producing a malformed scene.
        let mesh = aec_geometry::Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![], // empty
            uvs: vec![],     // empty
            indices: vec![0, 1, 2],
            attributes: vec![],
        };
        let mut s = RenderScene::new();
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        s.push_geometry_mesh("wall_1", &mesh, Some("oak".into()), identity);
        assert_eq!(s.meshes.len(), 1);
        let m = &s.meshes[0];
        assert_eq!(m.positions.len(), 3);
        assert_eq!(m.normals.len(), 3, "normals padded to position count");
        assert_eq!(m.uvs.len(), 3, "uvs padded to position count");
        assert_eq!(m.indices, vec![0, 1, 2]);
        assert_eq!(m.material_id, Some("oak".into()));
        assert_eq!(s.triangle_count(), 1);
    }

    #[test]
    fn push_geometry_mesh_preserves_provided_normals_and_uvs() {
        // When the geometry mesh DOES supply normals/uvs they must be
        // carried through verbatim — the path tracer relies on the
        // normals being unit-length and oriented per the source.
        let mesh = aec_geometry::Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            normals: vec![[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            uvs: vec![[0.0, 0.0], [1.0, 0.0]],
            indices: vec![],
            attributes: vec![],
        };
        let s = SerializedMesh::from_geometry_mesh("m", &mesh, None, [[0.0; 4]; 4]);
        assert_eq!(s.normals, vec![[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(s.uvs, vec![[0.0, 0.0], [1.0, 0.0]]);
    }
}
