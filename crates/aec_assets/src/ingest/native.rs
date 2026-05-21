//! Native bincode-serialised mesh blobs.
//!
//! Used by tests and by other Phase 9 crates that want to round-trip an
//! [`aec_geometry::Mesh`] through the asset pipeline without going via a
//! file-format reader. The on-disk format is whatever `bincode` produces
//! for the `Mesh` struct — a fast, schema-tied binary serialisation
//! that's only valid within this build of the workspace. (It is *not* a
//! shipping interchange format; external tooling should use glTF/OBJ.)

use aec_geometry::Mesh;

use super::{IngestError, IngestFormat, IngestedMesh};

/// Parse a native [`bincode`]-encoded [`Mesh`] blob.
pub fn parse(bytes: &[u8], source_label: impl Into<String>) -> Result<IngestedMesh, IngestError> {
    let mesh: Mesh =
        bincode::deserialize(bytes).map_err(|e| IngestError::Parse(format!("bincode: {e}")))?;
    if mesh.positions.is_empty() || mesh.indices.is_empty() {
        return Err(IngestError::Parse("native mesh has no geometry".into()));
    }
    Ok(IngestedMesh {
        mesh,
        format: IngestFormat::Native,
        source_label: source_label.into(),
    })
}

/// Encode a mesh as a native bincode blob.
pub fn encode(mesh: &Mesh) -> Vec<u8> {
    bincode::serialize(mesh).expect("bincode::serialize of Mesh is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> Mesh {
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
    }

    #[test]
    fn round_trip_preserves_geometry() {
        let original = quad();
        let bytes = encode(&original);
        let parsed = parse(&bytes, "test").unwrap();
        assert_eq!(parsed.format, IngestFormat::Native);
        assert_eq!(parsed.source_label, "test");
        assert_eq!(parsed.mesh.positions, original.positions);
        assert_eq!(parsed.mesh.indices, original.indices);
    }

    #[test]
    fn empty_mesh_rejected() {
        let mesh = Mesh::new();
        let bytes = encode(&mesh);
        let err = parse(&bytes, "empty").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn corrupt_bytes_rejected() {
        let err = parse(&[0xFF, 0xFF, 0xFF, 0xFF], "corrupt").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }
}
