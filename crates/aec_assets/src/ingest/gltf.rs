//! glTF 2.0 ingest (both `.gltf` JSON + `.glb` binary).
//!
//! We use the [`gltf`] crate (pure Rust) to parse the JSON/glb container
//! then walk the scene tree, baking each `Primitive` (with its world-
//! space node transform) into a single merged [`Mesh`]. Materials are
//! intentionally dropped — the asset pipeline carries materials
//! separately via `AssetMetadata::materials`.

use std::path::Path;

use aec_geometry::Mesh;
use glam::{Mat4, Vec3};

use super::{IngestError, IngestFormat, IngestedMesh};

/// Parse a glTF/glb byte slice. Handles both the JSON form (with
/// `data:` URI embedded buffers) and the binary GLB form. External
/// `.bin` buffers referenced by the JSON form cannot be resolved here —
/// callers with files on disk should use [`parse_path`] instead.
pub fn parse_bytes(
    bytes: &[u8],
    source_label: impl Into<String>,
    expected: IngestFormat,
) -> Result<IngestedMesh, IngestError> {
    let _ = expected; // detection is in detect_format; the import handles both
    let (doc, buffers, _images) = gltf::import_slice(bytes)
        .map_err(|e| IngestError::Parse(format!("gltf import_slice: {e}")))?;
    bake_document(&doc, &buffers, source_label.into())
}

/// Parse a glTF/glb path; external `.bin` buffers and embedded data URIs
/// are resolved relative to the file's directory.
pub fn parse_path(path: &Path, _expected: IngestFormat) -> Result<IngestedMesh, IngestError> {
    let (doc, buffers, _images) =
        gltf::import(path).map_err(|e| IngestError::Parse(format!("gltf import: {e}")))?;
    let label = path.to_string_lossy().to_string();
    bake_document(&doc, &buffers, label)
}

fn bake_document(
    doc: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    source_label: String,
) -> Result<IngestedMesh, IngestError> {
    let mut out = Mesh::new();
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or_else(|| IngestError::Parse("glTF has no scenes".into()))?;
    for node in scene.nodes() {
        walk_node(&node, Mat4::IDENTITY, buffers, &mut out)?;
    }
    if out.indices.is_empty() {
        return Err(IngestError::Parse("glTF has no triangle primitives".into()));
    }
    // glTF determines whether `.gltf` or `.glb` was used by the source —
    // surface that back via the source label, but tag the format as
    // `Gltf` either way (downstream code doesn't distinguish).
    let format = if source_label.to_ascii_lowercase().ends_with(".glb") {
        IngestFormat::Glb
    } else {
        IngestFormat::Gltf
    };
    Ok(IngestedMesh {
        mesh: out,
        format,
        source_label,
    })
}

fn walk_node(
    node: &gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut Mesh,
) -> Result<(), IngestError> {
    let local = node_transform(node);
    let world = parent * local;
    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            bake_primitive(&primitive, world, buffers, out)?;
        }
    }
    for child in node.children() {
        walk_node(&child, world, buffers, out)?;
    }
    Ok(())
}

fn node_transform(node: &gltf::Node) -> Mat4 {
    // gltf::Node::transform() returns a unified Trs or Matrix variant.
    let m = node.transform().matrix();
    Mat4::from_cols_array_2d(&m)
}

fn bake_primitive(
    primitive: &gltf::Primitive,
    world: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut Mesh,
) -> Result<(), IngestError> {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        // Skip lines/points/strips/fans; the asset pipeline is mesh-only.
        return Ok(());
    }
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or_else(|| IngestError::Parse("primitive missing POSITION".into()))?
        .collect();
    if positions.is_empty() {
        return Ok(());
    }
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
    let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|tc| tc.into_f32().collect());
    let indices: Vec<u32> = reader.read_indices().map_or_else(
        || (0..positions.len() as u32).collect(),
        |i| i.into_u32().collect(),
    );

    // Guard against singular world matrices (e.g. a zero-scale node in
    // the glTF scene tree). `inverse()` on a singular matrix produces
    // inf/NaN which propagates into positions and normals. Skip the
    // primitive entirely in that (pathological) case.
    if world.determinant().abs() < 1e-30 {
        return Ok(());
    }
    let normal_xform = world.inverse().transpose();
    let base = u32::try_from(out.positions.len())
        .map_err(|_| IngestError::Parse("vertex count exceeds u32".into()))?;

    for (i, p) in positions.iter().enumerate() {
        let p4 = world.transform_point3(Vec3::from_array(*p));
        out.positions.push(p4.into());
        let n = normals.as_ref().map_or([0.0, 0.0, 1.0], |ns| ns[i]);
        let n_world = normal_xform.transform_vector3(Vec3::from_array(n));
        let len = n_world.length();
        out.normals.push(if len > 1e-12 {
            (n_world / len).into()
        } else {
            [0.0, 0.0, 1.0]
        });
        out.uvs.push(uvs.as_ref().map_or([0.0, 0.0], |u| u[i]));
    }
    for chunk in indices.chunks_exact(3) {
        out.indices
            .extend_from_slice(&[base + chunk[0], base + chunk[1], base + chunk[2]]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum-viable glTF JSON: one node, one mesh, one triangle.
    const TRIANGLE_GLTF: &str = r#"{
  "asset": { "version": "2.0" },
  "scene": 0,
  "scenes": [{ "nodes": [0] }],
  "nodes": [{ "mesh": 0 }],
  "meshes": [{ "primitives": [{
    "attributes": { "POSITION": 0 },
    "indices": 1,
    "mode": 4
  }] }],
  "accessors": [
    { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
      "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] },
    { "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }
  ],
  "bufferViews": [
    { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
    { "buffer": 0, "byteOffset": 36, "byteLength": 6 }
  ],
  "buffers": [
    { "byteLength": 44, "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAEAAgA=" }
  ]
}"#;

    #[test]
    fn glb_magic_recognised() {
        // A minimal GLB header with empty JSON chunk + no binary.
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        // Build the smallest JSON chunk + total length.
        let json = br#"{"asset":{"version":"2.0"},"scenes":[],"nodes":[],"meshes":[]}"#;
        let pad = (4 - (json.len() % 4)) % 4;
        let mut padded = json.to_vec();
        padded.resize(padded.len() + pad, b' ');
        let chunk_len = padded.len() as u32;
        let total_len: u32 = 12 + 8 + chunk_len;
        glb.extend_from_slice(&total_len.to_le_bytes());
        // JSON chunk.
        glb.extend_from_slice(&chunk_len.to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&padded);
        // No scenes -> Err in bake_document, but we only need to verify
        // that the magic-byte recognition pipeline goes that far.
        let err = parse_bytes(&glb, "minimal.glb", IngestFormat::Glb).unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn glb_corrupt_rejected() {
        let err = parse_bytes(b"not-a-glb", "bad.glb", IngestFormat::Glb).unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn gltf_with_data_uri_triangle() {
        let ing = parse_bytes(
            TRIANGLE_GLTF.as_bytes(),
            "triangle.gltf",
            IngestFormat::Gltf,
        )
        .unwrap();
        assert_eq!(ing.format, IngestFormat::Gltf);
        assert_eq!(ing.mesh.triangle_count(), 1);
        assert_eq!(ing.mesh.positions.len(), 3);
        assert!((ing.mesh.positions[1][0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn gltf_without_scenes_rejected() {
        let json = r#"{"asset":{"version":"2.0"}}"#;
        let err = parse_bytes(json.as_bytes(), "no-scenes.gltf", IngestFormat::Gltf).unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }
}
