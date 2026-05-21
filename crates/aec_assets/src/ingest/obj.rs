//! Wavefront `.obj` ingest.
//!
//! Uses [`tobj`] (pure-Rust, no external libs) and merges all groups
//! into a single indexed [`Mesh`]. Per-vertex normals fall back to the
//! face normal when the OBJ doesn't ship `vn` lines; UVs fall back to
//! `(0, 0)` when missing.

use std::path::Path;

use aec_geometry::Mesh;

use super::{IngestError, IngestFormat, IngestedMesh};

/// Parse OBJ bytes (no MTL — materials are imported separately).
pub fn parse_bytes(
    bytes: &[u8],
    source_label: impl Into<String>,
) -> Result<IngestedMesh, IngestError> {
    let s = std::str::from_utf8(bytes)
        .map_err(|e| IngestError::Parse(format!("obj is not utf-8: {e}")))?;
    let mut reader = std::io::BufReader::new(s.as_bytes());
    let (models, _materials) = tobj::load_obj_buf(
        &mut reader,
        &tobj::LoadOptions {
            single_index: true,
            triangulate: true,
            ignore_points: true,
            ignore_lines: true,
        },
        |_| Err(tobj::LoadError::OpenFileFailed),
    )
    .map_err(|e| IngestError::Parse(format!("obj parse: {e}")))?;
    merge_models(models, source_label.into())
}

/// Parse an OBJ on disk; MTLs referenced from the OBJ are accepted but
/// their materials are discarded (we just need the mesh).
pub fn parse_path(path: &Path) -> Result<IngestedMesh, IngestError> {
    let (models, _materials_res) = tobj::load_obj(
        path,
        &tobj::LoadOptions {
            single_index: true,
            triangulate: true,
            ignore_points: true,
            ignore_lines: true,
        },
    )
    .map_err(|e| IngestError::Parse(format!("obj parse: {e}")))?;
    let label = path.to_string_lossy().to_string();
    merge_models(models, label)
}

fn merge_models(
    models: Vec<tobj::Model>,
    source_label: String,
) -> Result<IngestedMesh, IngestError> {
    let mut out = Mesh::new();
    for model in &models {
        let m = &model.mesh;
        if m.positions.len() % 3 != 0 {
            return Err(IngestError::Parse(format!(
                "model `{}` has malformed positions",
                model.name
            )));
        }
        let n_verts = m.positions.len() / 3;
        if n_verts == 0 {
            continue;
        }
        let has_normals = m.normals.len() == m.positions.len();
        let has_uvs = m.texcoords.len() == n_verts * 2;
        let base = u32::try_from(out.positions.len()).map_err(|_| {
            IngestError::Parse(format!("model `{}` exceeds u32 vertex range", model.name))
        })?;
        // Pre-extend; default normals/uvs are filled below.
        for i in 0..n_verts {
            out.positions.push([
                m.positions[i * 3],
                m.positions[i * 3 + 1],
                m.positions[i * 3 + 2],
            ]);
            out.normals.push(if has_normals {
                [m.normals[i * 3], m.normals[i * 3 + 1], m.normals[i * 3 + 2]]
            } else {
                [0.0, 0.0, 1.0]
            });
            out.uvs.push(if has_uvs {
                [m.texcoords[i * 2], m.texcoords[i * 2 + 1]]
            } else {
                [0.0, 0.0]
            });
        }
        // Append indices (re-based).
        for &idx in &m.indices {
            out.indices.push(base + idx);
        }
        // If no normals shipped, compute flat per-face normals and splat
        // them onto the three vertices of each triangle (tobj's
        // `single_index` mode replicates vertices per face so this is
        // safe and produces faceted shading).
        if !has_normals {
            let last_face = (m.indices.len() / 3) as u32;
            for face in 0..last_face {
                let i0 = base + m.indices[(face * 3) as usize];
                let i1 = base + m.indices[(face * 3 + 1) as usize];
                let i2 = base + m.indices[(face * 3 + 2) as usize];
                let p0 = out.positions[i0 as usize];
                let p1 = out.positions[i1 as usize];
                let p2 = out.positions[i2 as usize];
                let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
                let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
                let n = [
                    e1[1] * e2[2] - e1[2] * e2[1],
                    e1[2] * e2[0] - e1[0] * e2[2],
                    e1[0] * e2[1] - e1[1] * e2[0],
                ];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                let n = if len > 1e-12 {
                    [n[0] / len, n[1] / len, n[2] / len]
                } else {
                    [0.0, 0.0, 1.0]
                };
                out.normals[i0 as usize] = n;
                out.normals[i1 as usize] = n;
                out.normals[i2 as usize] = n;
            }
        }
    }
    if out.indices.is_empty() {
        return Err(IngestError::Parse(
            "obj has no triangles after merge".into(),
        ));
    }
    Ok(IngestedMesh {
        mesh: out,
        format: IngestFormat::Obj,
        source_label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGLE_OBJ: &str = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 0.0 1.0 0.0
vn 0.0 0.0 1.0
vn 0.0 0.0 1.0
vn 0.0 0.0 1.0
vt 0.0 0.0
vt 1.0 0.0
vt 0.0 1.0
f 1/1/1 2/2/2 3/3/3
";

    const QUAD_OBJ: &str = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 1.0 1.0 0.0
v 0.0 1.0 0.0
f 1 2 3 4
";

    #[test]
    fn triangle_obj_parses_with_normals_and_uvs() {
        let ing = parse_bytes(TRIANGLE_OBJ.as_bytes(), "triangle.obj").unwrap();
        assert_eq!(ing.format, IngestFormat::Obj);
        assert_eq!(ing.mesh.triangle_count(), 1);
        assert_eq!(ing.mesh.positions.len(), 3);
        assert_eq!(ing.mesh.normals.len(), 3);
        assert!((ing.mesh.normals[0][2] - 1.0).abs() < 1e-6);
        assert_eq!(ing.mesh.uvs[2], [0.0, 1.0]);
    }

    #[test]
    fn quad_triangulates_to_two_faces() {
        let ing = parse_bytes(QUAD_OBJ.as_bytes(), "quad.obj").unwrap();
        assert_eq!(ing.mesh.triangle_count(), 2);
    }

    #[test]
    fn obj_without_normals_gets_flat_normals() {
        let ing = parse_bytes(QUAD_OBJ.as_bytes(), "quad.obj").unwrap();
        // Quad in the XY plane -> z normal.
        for n in &ing.mesh.normals {
            assert!((n[2].abs() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn empty_obj_rejected() {
        let err = parse_bytes(b"", "empty.obj").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn invalid_utf8_rejected() {
        let err = parse_bytes(&[0xFF, 0xFE, 0xFD], "bad.obj").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }
}
