//! IFC (ISO-10303-21) ingest.
//!
//! IFC files are whole-building parametric models, not single-asset
//! exchanges. We support the case that matters for the asset pipeline:
//! a furniture / fixture / component export that ships a `IFCFACETEDBREP`
//! polyhedron with its vertices inline. We:
//!
//! 1. Validate the file via [`aec_bim::ifc::IfcReader::from_string`],
//!    confirming the envelope and schema header parse.
//! 2. Walk the raw [`StepRecord`] stream from the same reader to find
//!    the lowest-level geometry primitives (`IFCCARTESIANPOINT`,
//!    `IFCPOLYLOOP`, `IFCFACEOUTERBOUND`, `IFCFACE`, `IFCCLOSEDSHELL`,
//!    `IFCFACETEDBREP`).
//! 3. Build [`aec_bim::tessellator::FacetedBrep`] structures from the
//!    resolved entities and call the existing tessellator.
//! 4. Merge the resulting meshes into one [`aec_geometry::Mesh`].
//!
//! Anything beyond a `IFCFACETEDBREP` (parametric extrusions, boolean
//! clipping, NURBS) is *out of scope for ingest*. Callers that need to
//! ingest a parametric IFC element should build a
//! [`aec_bim::tessellator::ExtrudedAreaSolid`] manually and pass it via
//! [`tessellator_mesh_to_geometry_mesh`].

use std::collections::HashMap;
use std::io::BufReader;

use aec_bim::ifc::{IfcReader, StepRecord};
use aec_bim::tessellator::{BrepFace, FacetedBrep, Mesh as TessMesh};
use aec_geometry::Mesh;

use super::{IngestError, IngestFormat, IngestedMesh};

/// Parse IFC bytes; the file must contain at least one geometry-bearing
/// entity (`IFCFACETEDBREP` or `IFCCLOSEDSHELL`).
///
/// We validate the ISO-10303-21 envelope manually rather than going via
/// [`IfcReader::from_string`] because asset-ingest IFC files often
/// contain only geometry (e.g. furniture exports from IfcOpenShell) and
/// lack the `IfcProject` / `IfcSite` / `IfcBuilding` hierarchy the high-
/// level reader requires.
pub fn parse_bytes(
    bytes: &[u8],
    source_label: impl Into<String>,
) -> Result<IngestedMesh, IngestError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| IngestError::Parse(format!("ifc not utf-8: {e}")))?;
    if !text.contains("ISO-10303-21;")
        || !text.contains("HEADER;")
        || !text.contains("DATA;")
        || !text.contains("ENDSEC;")
    {
        return Err(IngestError::Parse(
            "ifc envelope (ISO-10303-21 / HEADER / DATA / ENDSEC) is malformed".into(),
        ));
    }

    // Iterate the raw STEP records to extract geometry entities.
    let reader = BufReader::new(text.as_bytes());
    let mut records: HashMap<u32, StepRecord> = HashMap::new();
    for item in IfcReader::iter(reader) {
        match item {
            Ok(rec) => {
                records.insert(rec.step_id, rec);
            }
            Err(e) => return Err(IngestError::Parse(format!("step iter: {e}"))),
        }
    }

    let mut out = Mesh::new();
    let mut shells_found: u32 = 0;
    // Track shells consumed by an enclosing IFCFACETEDBREP so we don't
    // double-tessellate them as standalone IFCCLOSEDSHELL entities below.
    let mut consumed_shells: std::collections::HashSet<u32> = std::collections::HashSet::new();
    // Iterate in deterministic (step_id) order so the output mesh has
    // reproducible face ordering — important because the downstream
    // BLAKE3 content hash must be stable across runs.
    let mut sorted_ids: Vec<u32> = records.keys().copied().collect();
    sorted_ids.sort_unstable();
    for &id in &sorted_ids {
        let rec = &records[&id];
        if rec.kind == "IFCFACETEDBREP" {
            if let Some(shell_id) = rec.args.first().and_then(|s| parse_entity_ref(s)) {
                consumed_shells.insert(shell_id);
            }
            if let Some(brep) = resolve_brep(rec, &records) {
                if let Ok(mesh) = brep.tessellate() {
                    append_tess(&mut out, &mesh);
                    shells_found += 1;
                }
            }
        }
    }
    // Also pick up bare IFCCLOSEDSHELL entities (some exporters omit the
    // outer `IFCFACETEDBREP` wrapper when the model is a single shell).
    for &id in &sorted_ids {
        let rec = &records[&id];
        if rec.kind == "IFCCLOSEDSHELL" && !consumed_shells.contains(&rec.step_id) {
            if let Some(faces) = resolve_closed_shell(rec, &records) {
                let brep = FacetedBrep { faces };
                if let Ok(mesh) = brep.tessellate() {
                    append_tess(&mut out, &mesh);
                    shells_found += 1;
                }
            }
        }
    }
    if shells_found == 0 || out.indices.is_empty() {
        return Err(IngestError::Parse(
            "ifc has no IFCFACETEDBREP or IFCCLOSEDSHELL entities to tessellate".into(),
        ));
    }
    Ok(IngestedMesh {
        mesh: out,
        format: IngestFormat::Ifc,
        source_label: source_label.into(),
    })
}

/// Public helper: convert a [`aec_bim::tessellator::Mesh`] (positions
/// `[f64; 3]`, packed `[u32; 3]` indices) into a
/// [`aec_geometry::Mesh`] (positions `[f32; 3]`, flat `Vec<u32>` indices)
/// with per-vertex flat-shading normals computed from face geometry.
pub fn tessellator_mesh_to_geometry_mesh(tess: &TessMesh) -> Mesh {
    let mut out = Mesh::new();
    for tri in &tess.indices {
        let p0 = tess.positions[tri[0] as usize];
        let p1 = tess.positions[tri[1] as usize];
        let p2 = tess.positions[tri[2] as usize];
        let n = face_normal(p0, p1, p2);
        let base = out.positions.len() as u32;
        for p in [p0, p1, p2] {
            out.positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            out.normals.push(n);
            out.uvs.push([0.0, 0.0]);
        }
        out.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    out
}

fn append_tess(out: &mut Mesh, tess: &TessMesh) {
    for tri in &tess.indices {
        let p0 = tess.positions[tri[0] as usize];
        let p1 = tess.positions[tri[1] as usize];
        let p2 = tess.positions[tri[2] as usize];
        let n = face_normal(p0, p1, p2);
        let base = out.positions.len() as u32;
        for p in [p0, p1, p2] {
            out.positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            out.normals.push(n);
            out.uvs.push([0.0, 0.0]);
        }
        out.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
}

fn face_normal(p0: [f64; 3], p1: [f64; 3], p2: [f64; 3]) -> [f32; 3] {
    let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
    let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
    let n = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-12 {
        [
            (n[0] / len) as f32,
            (n[1] / len) as f32,
            (n[2] / len) as f32,
        ]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn resolve_brep(rec: &StepRecord, records: &HashMap<u32, StepRecord>) -> Option<FacetedBrep> {
    // IFCFACETEDBREP(#OUTER)  -> outer is a IFCCLOSEDSHELL
    let outer_ref = parse_entity_ref(rec.args.first()?)?;
    let outer = records.get(&outer_ref)?;
    if outer.kind != "IFCCLOSEDSHELL" {
        return None;
    }
    let faces = resolve_closed_shell(outer, records)?;
    Some(FacetedBrep { faces })
}

fn resolve_closed_shell(
    rec: &StepRecord,
    records: &HashMap<u32, StepRecord>,
) -> Option<Vec<BrepFace>> {
    // IFCCLOSEDSHELL((#F1, #F2, ...))
    let list = parse_entity_ref_list(rec.args.first()?)?;
    let mut faces = Vec::new();
    for face_id in list {
        let face_rec = records.get(&face_id)?;
        if face_rec.kind != "IFCFACE" {
            continue;
        }
        if let Some(face) = resolve_face(face_rec, records) {
            faces.push(face);
        }
    }
    if faces.is_empty() {
        return None;
    }
    Some(faces)
}

fn resolve_face(rec: &StepRecord, records: &HashMap<u32, StepRecord>) -> Option<BrepFace> {
    // IFCFACE((#BOUND_1, ...))
    // The aec_bim BrepFace stores a single ring with no inner-loop
    // support, so we only resolve the outer bound here. IFCFACEBOUND
    // entries (inner holes) are silently dropped — assets with holes in
    // their topology should export as glTF or OBJ instead.
    let bounds = parse_entity_ref_list(rec.args.first()?)?;
    for b_id in bounds {
        let b_rec = records.get(&b_id)?;
        if b_rec.kind == "IFCFACEOUTERBOUND" {
            if let Some(verts) = resolve_loop(b_rec, records) {
                return Some(BrepFace { loop_: verts });
            }
        }
    }
    None
}

fn resolve_loop(rec: &StepRecord, records: &HashMap<u32, StepRecord>) -> Option<Vec<[f64; 3]>> {
    // IFCFACEOUTERBOUND(#LOOP, .T.)  /  IFCFACEBOUND(#LOOP, .T.)
    let loop_id = parse_entity_ref(rec.args.first()?)?;
    let polyloop = records.get(&loop_id)?;
    if polyloop.kind != "IFCPOLYLOOP" {
        return None;
    }
    let point_ids = parse_entity_ref_list(polyloop.args.first()?)?;
    let mut verts = Vec::with_capacity(point_ids.len());
    for pid in point_ids {
        let pt = records.get(&pid)?;
        if pt.kind != "IFCCARTESIANPOINT" {
            return None;
        }
        let coords = parse_real_triple(pt.args.first()?)?;
        verts.push(coords);
    }
    if verts.len() < 3 {
        return None;
    }
    Some(verts)
}

/// Parse `"#123"` -> `123`. Returns `None` on any other shape.
fn parse_entity_ref(s: &str) -> Option<u32> {
    s.strip_prefix('#').and_then(|n| n.parse::<u32>().ok())
}

/// Parse `"(#1, #2, #3)"` -> `vec![1, 2, 3]`. Whitespace tolerant.
fn parse_entity_ref_list(s: &str) -> Option<Vec<u32>> {
    let s = s.trim();
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let mut out = Vec::new();
    for tok in inner.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        out.push(parse_entity_ref(t)?);
    }
    Some(out)
}

/// Parse `"(1.0, 2.0, 3.0)"` -> `[1.0, 2.0, 3.0]`. Whitespace tolerant.
fn parse_real_triple(s: &str) -> Option<[f64; 3]> {
    let s = s.trim();
    let inner = s.strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.split(',');
    let a: f64 = parts.next()?.trim().parse().ok()?;
    let b: f64 = parts.next()?.trim().parse().ok()?;
    let c: f64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([a, b, c])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimum-viable IFC file containing a unit tetrahedron
    /// modelled as an `IFCFACETEDBREP`. The envelope is the smallest
    /// possible STEP wrapper that passes the high-level `IfcReader`.
    fn tetra_ifc() -> String {
        // Header + 4 cartesian points + 4 face bounds + 4 faces + shell + brep.
        let mut s = String::new();
        s.push_str("ISO-10303-21;\n");
        s.push_str("HEADER;\n");
        s.push_str("FILE_DESCRIPTION(('asset'),'2;1');\n");
        s.push_str(
            "FILE_NAME('tetra.ifc','2024-01-01T00:00:00',('aec'),('aec'),'aec','aec','');\n",
        );
        s.push_str("FILE_SCHEMA(('IFC4'));\n");
        s.push_str("ENDSEC;\n");
        s.push_str("DATA;\n");
        // Tetra verts.
        s.push_str("#1=IFCCARTESIANPOINT((0.0,0.0,0.0));\n");
        s.push_str("#2=IFCCARTESIANPOINT((1.0,0.0,0.0));\n");
        s.push_str("#3=IFCCARTESIANPOINT((0.0,1.0,0.0));\n");
        s.push_str("#4=IFCCARTESIANPOINT((0.0,0.0,1.0));\n");
        // Polyloops.
        s.push_str("#10=IFCPOLYLOOP((#1,#3,#2));\n");
        s.push_str("#11=IFCPOLYLOOP((#1,#2,#4));\n");
        s.push_str("#12=IFCPOLYLOOP((#2,#3,#4));\n");
        s.push_str("#13=IFCPOLYLOOP((#3,#1,#4));\n");
        // Outer bounds.
        s.push_str("#20=IFCFACEOUTERBOUND(#10,.T.);\n");
        s.push_str("#21=IFCFACEOUTERBOUND(#11,.T.);\n");
        s.push_str("#22=IFCFACEOUTERBOUND(#12,.T.);\n");
        s.push_str("#23=IFCFACEOUTERBOUND(#13,.T.);\n");
        // Faces.
        s.push_str("#30=IFCFACE((#20));\n");
        s.push_str("#31=IFCFACE((#21));\n");
        s.push_str("#32=IFCFACE((#22));\n");
        s.push_str("#33=IFCFACE((#23));\n");
        // Closed shell + brep.
        s.push_str("#40=IFCCLOSEDSHELL((#30,#31,#32,#33));\n");
        s.push_str("#41=IFCFACETEDBREP(#40);\n");
        s.push_str("ENDSEC;\n");
        s.push_str("END-ISO-10303-21;\n");
        s
    }

    #[test]
    fn tetrahedron_ifc_ingests_to_four_triangles() {
        let ifc = tetra_ifc();
        let ing = parse_bytes(ifc.as_bytes(), "tetra.ifc").unwrap();
        assert_eq!(ing.format, IngestFormat::Ifc);
        assert_eq!(ing.mesh.triangle_count(), 4);
        assert_eq!(ing.mesh.positions.len(), 12); // 3 per triangle (flat shading)
    }

    #[test]
    fn parse_entity_ref_basic() {
        assert_eq!(parse_entity_ref("#42"), Some(42));
        assert_eq!(parse_entity_ref("$"), None);
        assert_eq!(parse_entity_ref("42"), None);
    }

    #[test]
    fn parse_entity_ref_list_basic() {
        assert_eq!(parse_entity_ref_list("(#1, #2, #3)"), Some(vec![1, 2, 3]));
        assert_eq!(parse_entity_ref_list("(#1)"), Some(vec![1]));
        assert_eq!(parse_entity_ref_list("()"), Some(Vec::new()));
        assert_eq!(parse_entity_ref_list("malformed"), None);
    }

    #[test]
    fn parse_real_triple_basic() {
        let v = parse_real_triple("(1.5, 2.0, -3.25)").unwrap();
        assert!((v[0] - 1.5).abs() < 1e-9);
        assert!((v[1] - 2.0).abs() < 1e-9);
        assert!((v[2] - (-3.25)).abs() < 1e-9);
        assert!(parse_real_triple("(1.0, 2.0)").is_none());
        assert!(parse_real_triple("(1.0, 2.0, 3.0, 4.0)").is_none());
    }

    #[test]
    fn ifc_without_geometry_rejected() {
        let mut ifc = tetra_ifc();
        // Remove the brep and shell.
        ifc = ifc.replace("#40=IFCCLOSEDSHELL((#30,#31,#32,#33));\n", "");
        ifc = ifc.replace("#41=IFCFACETEDBREP(#40);\n", "");
        let err = parse_bytes(ifc.as_bytes(), "no-geom.ifc").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn invalid_utf8_rejected() {
        let err = parse_bytes(&[0xFF, 0xFE, 0xFD], "bad.ifc").unwrap_err();
        assert!(matches!(err, IngestError::Parse(_)));
    }

    #[test]
    fn tessellator_mesh_conversion_preserves_triangle_count() {
        let tess = TessMesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            indices: vec![[0, 1, 2]],
        };
        let m = tessellator_mesh_to_geometry_mesh(&tess);
        assert_eq!(m.triangle_count(), 1);
        assert_eq!(m.positions.len(), 3);
        assert!((m.normals[0][2] - 1.0).abs() < 1e-6);
    }
}
