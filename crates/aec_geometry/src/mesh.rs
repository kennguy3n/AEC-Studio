//! Tiny indexed mesh format used by the viewport (wgpu rasteriser) and
//! the native CPU/GPU path tracer. Vertices are positions in
//! millimeters, normals are unit vectors, UVs are in `[0,1]`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mesh {
    /// Interleaved as `[x, y, z]` per vertex.
    pub positions: Vec<[f32; 3]>,
    /// One per vertex, parallel to `positions`.
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    #[serde(default)]
    pub attributes: Vec<MeshAttribute>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshAttribute {
    pub name: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy)]
pub struct Triangle {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub c: [f32; 3],
}

impl Mesh {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Append a quad `[a, b, c, d]` (assumed CCW seen from `normal`).
    pub fn push_quad(
        &mut self,
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
        d: [f32; 3],
        normal: [f32; 3],
        uvs: [[f32; 2]; 4],
    ) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&[a, b, c, d]);
        self.normals.extend_from_slice(&[normal; 4]);
        self.uvs.extend_from_slice(&uvs);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Append a single triangle.
    pub fn push_triangle(
        &mut self,
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
        normal: [f32; 3],
        uvs: [[f32; 2]; 3],
    ) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&[a, b, c]);
        self.normals.extend_from_slice(&[normal; 3]);
        self.uvs.extend_from_slice(&uvs);
        self.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    /// Iterate world-space triangles, useful for the spatial index.
    pub fn triangles(&self) -> impl Iterator<Item = Triangle> + '_ {
        self.indices.chunks_exact(3).map(move |idx| Triangle {
            a: self.positions[idx[0] as usize],
            b: self.positions[idx[1] as usize],
            c: self.positions[idx[2] as usize],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_emits_two_triangles() {
        let mut m = Mesh::new();
        m.push_quad(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        );
        assert_eq!(m.triangle_count(), 2);
        assert_eq!(m.vertex_count(), 4);
        assert_eq!(m.triangles().count(), 2);
    }
}
