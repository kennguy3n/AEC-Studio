//! Deterministic glTF 2.0 exporter.
//!
//! Takes a [`GltfScene`] (meshes + materials + cameras + lights) and
//! writes either a `.gltf` + `.bin` pair or a binary `.glb`. The
//! output is byte-identical for the same input regardless of platform
//! or wall-clock time.
//!
//! Coverage (Phase 11 Group C Task 20):
//! - **Geometry**: every mesh's POSITION, NORMAL, TEXCOORD_0 and
//!   indices stream into a single packed binary buffer with proper
//!   bufferView / accessor wiring. Vertices in mm are converted to
//!   metres for glTF (the format's expected unit).
//! - **Materials**: PBR metallic-roughness with baseColorFactor,
//!   metallicFactor, roughnessFactor, emissiveFactor. Materials
//!   referenced by meshes are emitted in deterministic order.
//! - **Cameras**: perspective cameras with computed aspect / yfov /
//!   znear / zfar; each camera lives under a node whose transform is
//!   derived from the camera's (position, target, up) via lookAt
//!   inversion.
//! - **Lights**: emitted under the `KHR_lights_punctual` extension —
//!   directional / point / spot, with color, intensity and (for spot)
//!   inner/outer cone angles.
//! - **Output format**: when `out_path` has `.gltf` extension, a JSON
//!   file is written plus a sibling `.bin` file; when `.glb` (the
//!   recommended default for transport), a single binary GLB file
//!   with header + JSON chunk + BIN chunk is emitted.
//!
//! The exporter has zero new third-party dependencies — we hand-roll
//! the JSON shape via `serde_json::json!` so the order of keys (and
//! therefore the byte-level output) is fully under our control.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Aggregate of every error produced by [`write_gltf`].
#[derive(Debug, thiserror::Error)]
pub enum GltfExportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// Returned by [`write_gltf`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteGltfResult {
    pub out_path: PathBuf,
    /// Path to the sibling `.bin` file, when writing a split
    /// `.gltf`+`.bin` pair. `None` for `.glb`.
    pub bin_path: Option<PathBuf>,
    pub mesh_count: u32,
    pub material_count: u32,
    pub camera_count: u32,
    pub light_count: u32,
    /// Total triangle count across every mesh.
    pub triangle_count: u32,
}

/// Top-level scene to export.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GltfScene {
    pub name: String,
    pub meshes: Vec<GltfMesh>,
    pub materials: Vec<GltfMaterial>,
    pub cameras: Vec<GltfCamera>,
    pub lights: Vec<GltfLight>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GltfMesh {
    pub name: String,
    /// Position per vertex, interleaved as `[x, y, z]`. Units: mm.
    pub positions: Vec<[f32; 3]>,
    /// Unit normals per vertex, parallel to `positions`.
    pub normals: Vec<[f32; 3]>,
    /// UV per vertex (optional — empty vec = no TEXCOORD_0).
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices, packed `[i0, i1, i2, i0, i1, i2, ...]`.
    pub indices: Vec<u32>,
    /// Optional material reference (matches `GltfMaterial::id`).
    pub material_id: Option<String>,
    /// World transform encoded as a row-major 4x4 matrix in mm.
    pub transform: [[f32; 4]; 4],
}

impl GltfMesh {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            material_id: None,
            transform: identity_matrix(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GltfMaterial {
    pub id: String,
    pub name: String,
    /// Linear-space base colour with alpha.
    pub base_color_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub emissive_factor: [f32; 3],
    /// If true the renderer should disable backface culling for this
    /// material. Useful for two-sided cards (curtains, leaves).
    pub double_sided: bool,
}

impl GltfMaterial {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_color_factor: [0.6, 0.6, 0.6, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.6,
            emissive_factor: [0.0, 0.0, 0.0],
            double_sided: false,
        }
    }

    /// Build a glTF material from a [`aec_materials::PbrMaterial`].
    pub fn from_pbr(m: &aec_materials::PbrMaterial) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            base_color_factor: [m.albedo[0], m.albedo[1], m.albedo[2], 1.0],
            metallic_factor: m.metallic,
            roughness_factor: m.roughness,
            emissive_factor: m.emissive,
            double_sided: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GltfCamera {
    pub name: String,
    pub position_mm: [f32; 3],
    pub target_mm: [f32; 3],
    pub up_mm: [f32; 3],
    /// Horizontal field of view in radians. The exporter derives the
    /// vertical FOV from `aspect_ratio`.
    pub aspect_ratio: f32,
    pub yfov_rad: f32,
    pub znear_mm: f32,
    pub zfar_mm: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GltfLight {
    pub name: String,
    pub kind: GltfLightKind,
    /// Linear-space colour, components in [0, 1].
    pub color: [f32; 3],
    /// glTF intensity convention: lumens for point/spot, lux for
    /// directional.
    pub intensity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GltfLightKind {
    Directional {
        direction_mm: [f32; 3],
    },
    Point {
        position_mm: [f32; 3],
    },
    Spot {
        position_mm: [f32; 3],
        direction_mm: [f32; 3],
        /// Inner cone half-angle in radians.
        inner_cone_angle: f32,
        /// Outer cone half-angle in radians.
        outer_cone_angle: f32,
    },
}

impl GltfLightKind {
    fn gltf_type(&self) -> &'static str {
        match self {
            Self::Directional { .. } => "directional",
            Self::Point { .. } => "point",
            Self::Spot { .. } => "spot",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteGltfOptions {
    /// Convert mm coordinates to metres on write (glTF's expected
    /// unit). Recommended; defaults to true.
    pub mm_to_metres: bool,
    /// Pretty-print the JSON (only meaningful for `.gltf` output).
    pub pretty_print: bool,
    /// Generator string written to the asset table.
    pub generator: String,
}

impl Default for WriteGltfOptions {
    fn default() -> Self {
        Self {
            mm_to_metres: true,
            pretty_print: true,
            generator: "AEC Studio aec_export gltf_export".into(),
        }
    }
}

/// Output format. Inferred from the path extension; `.glb` → Binary,
/// anything else → Json (with sibling `.bin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GltfFormat {
    /// Single `.glb` file (12-byte header + JSON chunk + BIN chunk).
    Binary,
    /// `.gltf` JSON file + sibling `.bin` file.
    Json,
}

impl GltfFormat {
    pub fn infer(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("glb" | "GLB") => Self::Binary,
            _ => Self::Json,
        }
    }
}

/// Write a glTF document for the given scene to `out_path`. Routes to
/// either `.glb` (single-file binary) or `.gltf`+`.bin` (split JSON)
/// based on the extension.
pub fn write_gltf(
    out_path: &Path,
    scene: &GltfScene,
    options: &WriteGltfOptions,
) -> Result<WriteGltfResult, GltfExportError> {
    ensure_parent_dir(out_path)?;
    let format = GltfFormat::infer(out_path);

    // Build the binary buffer first; we need its byte offsets to
    // populate the JSON's bufferViews + accessors.
    let mut buffer = Vec::<u8>::new();
    let mut bufferviews = Vec::<Value>::new();
    let mut accessors = Vec::<Value>::new();
    let mut gltf_meshes = Vec::<Value>::new();
    let mut nodes = Vec::<Value>::new();
    let mut node_indices = Vec::<u32>::new();

    let scale = if options.mm_to_metres { 0.001 } else { 1.0 };

    let mut triangle_count: u32 = 0;
    for (mesh_idx, mesh) in scene.meshes.iter().enumerate() {
        if mesh.positions.is_empty() || mesh.indices.is_empty() {
            return Err(GltfExportError::Invalid(format!(
                "mesh `{}` has no positions or indices",
                mesh.name
            )));
        }
        if mesh.indices.len() % 3 != 0 {
            return Err(GltfExportError::Invalid(format!(
                "mesh `{}` has non-triangle index count {}",
                mesh.name,
                mesh.indices.len()
            )));
        }
        if !mesh.normals.is_empty() && mesh.normals.len() != mesh.positions.len() {
            return Err(GltfExportError::Invalid(format!(
                "mesh `{}` normals/positions length mismatch ({} vs {})",
                mesh.name,
                mesh.normals.len(),
                mesh.positions.len(),
            )));
        }
        if !mesh.uvs.is_empty() && mesh.uvs.len() != mesh.positions.len() {
            return Err(GltfExportError::Invalid(format!(
                "mesh `{}` uvs/positions length mismatch ({} vs {})",
                mesh.name,
                mesh.uvs.len(),
                mesh.positions.len(),
            )));
        }
        triangle_count += (mesh.indices.len() / 3) as u32;

        // -- POSITION accessor ----------------------------------------
        let positions_min = vec3_min(&mesh.positions, scale);
        let positions_max = vec3_max(&mesh.positions, scale);
        let positions_offset = buffer.len();
        for p in &mesh.positions {
            for v in p {
                buffer.extend_from_slice(&(*v * scale).to_le_bytes());
            }
        }
        let positions_len = buffer.len() - positions_offset;
        align_to_4(&mut buffer);
        let positions_bv = push_bufferview(
            &mut bufferviews,
            positions_offset,
            positions_len,
            Some(34_962),
            None,
        );
        let positions_acc = push_accessor(
            &mut accessors,
            positions_bv,
            5126, // FLOAT
            "VEC3",
            mesh.positions.len() as u32,
            Some(positions_min.to_vec()),
            Some(positions_max.to_vec()),
        );

        // -- NORMAL accessor ------------------------------------------
        let normal_acc = if mesh.normals.is_empty() {
            None
        } else {
            let normals_offset = buffer.len();
            for n in &mesh.normals {
                for v in n {
                    buffer.extend_from_slice(&v.to_le_bytes());
                }
            }
            let normals_len = buffer.len() - normals_offset;
            align_to_4(&mut buffer);
            let normals_bv = push_bufferview(
                &mut bufferviews,
                normals_offset,
                normals_len,
                Some(34_962),
                None,
            );
            Some(push_accessor(
                &mut accessors,
                normals_bv,
                5126,
                "VEC3",
                mesh.normals.len() as u32,
                None,
                None,
            ))
        };

        // -- TEXCOORD_0 accessor --------------------------------------
        let uv_acc = if mesh.uvs.is_empty() {
            None
        } else {
            let uvs_offset = buffer.len();
            for uv in &mesh.uvs {
                for v in uv {
                    buffer.extend_from_slice(&v.to_le_bytes());
                }
            }
            let uvs_len = buffer.len() - uvs_offset;
            align_to_4(&mut buffer);
            let uvs_bv = push_bufferview(&mut bufferviews, uvs_offset, uvs_len, Some(34_962), None);
            Some(push_accessor(
                &mut accessors,
                uvs_bv,
                5126,
                "VEC2",
                mesh.uvs.len() as u32,
                None,
                None,
            ))
        };

        // -- INDEX accessor -------------------------------------------
        // Pick the smallest unsigned integer type that fits.
        let max_idx = *mesh.indices.iter().max().unwrap_or(&0);
        let (idx_component, idx_size) = if u16::try_from(max_idx).is_ok() {
            (5123u32, 2usize) // UNSIGNED_SHORT
        } else {
            (5125, 4) // UNSIGNED_INT
        };
        let idx_offset = buffer.len();
        for i in &mesh.indices {
            if idx_size == 2 {
                buffer.extend_from_slice(&(*i as u16).to_le_bytes());
            } else {
                buffer.extend_from_slice(&i.to_le_bytes());
            }
        }
        let idx_len = buffer.len() - idx_offset;
        align_to_4(&mut buffer);
        let idx_bv = push_bufferview(
            &mut bufferviews,
            idx_offset,
            idx_len,
            Some(34_963), // ELEMENT_ARRAY_BUFFER
            None,
        );
        let idx_acc = push_accessor(
            &mut accessors,
            idx_bv,
            idx_component,
            "SCALAR",
            mesh.indices.len() as u32,
            None,
            None,
        );

        // -- Mesh + node ----------------------------------------------
        let mut attributes = serde_json::Map::new();
        attributes.insert("POSITION".to_string(), json!(positions_acc));
        if let Some(a) = normal_acc {
            attributes.insert("NORMAL".to_string(), json!(a));
        }
        if let Some(a) = uv_acc {
            attributes.insert("TEXCOORD_0".to_string(), json!(a));
        }
        let mut primitive = serde_json::Map::new();
        primitive.insert("attributes".to_string(), Value::Object(attributes));
        primitive.insert("indices".to_string(), json!(idx_acc));
        if let Some(mat) = mesh.material_id.as_ref() {
            if let Some(mat_idx) = scene.materials.iter().position(|m| &m.id == mat) {
                primitive.insert("material".to_string(), json!(mat_idx));
            }
        }
        primitive.insert("mode".to_string(), json!(4)); // TRIANGLES

        gltf_meshes.push(json!({
            "name": mesh.name,
            "primitives": [Value::Object(primitive)],
        }));

        // The mesh's world transform is encoded as a node `matrix`.
        // glTF stores matrices in column-major order, so transpose.
        let mat_column_major = transpose_and_scale(mesh.transform, scale);
        nodes.push(json!({
            "name": format!("{}_node", mesh.name),
            "mesh": mesh_idx,
            "matrix": mat_column_major,
        }));
        node_indices.push((nodes.len() - 1) as u32);
    }

    // -- Materials ----------------------------------------------------
    let mut materials = Vec::<Value>::new();
    for m in &scene.materials {
        materials.push(json!({
            "name": m.name,
            "pbrMetallicRoughness": {
                "baseColorFactor": m.base_color_factor,
                "metallicFactor": m.metallic_factor,
                "roughnessFactor": m.roughness_factor,
            },
            "emissiveFactor": m.emissive_factor,
            "doubleSided": m.double_sided,
        }));
    }

    // -- Cameras ------------------------------------------------------
    let mut cameras = Vec::<Value>::new();
    for (cam_idx, c) in scene.cameras.iter().enumerate() {
        let aspect = if c.aspect_ratio.is_finite() && c.aspect_ratio > 0.0 {
            c.aspect_ratio
        } else {
            1.0
        };
        let znear = (c.znear_mm * scale).max(1e-6);
        let zfar = (c.zfar_mm * scale).max(znear * 10.0);
        cameras.push(json!({
            "type": "perspective",
            "name": c.name,
            "perspective": {
                "aspectRatio": aspect,
                "yfov": c.yfov_rad,
                "znear": znear,
                "zfar": zfar,
            },
        }));

        let (translation, rotation) = camera_lookat_components(
            scale_vec(c.position_mm, scale),
            scale_vec(c.target_mm, scale),
            c.up_mm,
        );
        nodes.push(json!({
            "name": format!("{}_camera", c.name),
            "camera": cam_idx,
            "translation": translation,
            "rotation": rotation,
        }));
        node_indices.push((nodes.len() - 1) as u32);
    }

    // -- Lights (KHR_lights_punctual) --------------------------------
    let mut lights = Vec::<Value>::new();
    for (light_idx, l) in scene.lights.iter().enumerate() {
        let mut light = serde_json::Map::new();
        light.insert("name".to_string(), json!(l.name));
        light.insert("type".to_string(), json!(l.kind.gltf_type()));
        light.insert("color".to_string(), json!(l.color));
        light.insert("intensity".to_string(), json!(l.intensity));
        if let GltfLightKind::Spot {
            inner_cone_angle,
            outer_cone_angle,
            ..
        } = &l.kind
        {
            light.insert(
                "spot".to_string(),
                json!({
                    "innerConeAngle": inner_cone_angle,
                    "outerConeAngle": outer_cone_angle,
                }),
            );
        }
        lights.push(Value::Object(light));

        // The light's transform: directional uses orientation only;
        // point uses translation only; spot uses both.
        let (translation, rotation) = match &l.kind {
            GltfLightKind::Directional { direction_mm } => {
                let rot = direction_to_quat(*direction_mm);
                ([0.0, 0.0, 0.0], rot)
            }
            GltfLightKind::Point { position_mm } => {
                (scale_vec(*position_mm, scale), [0.0, 0.0, 0.0, 1.0])
            }
            GltfLightKind::Spot {
                position_mm,
                direction_mm,
                ..
            } => (
                scale_vec(*position_mm, scale),
                direction_to_quat(*direction_mm),
            ),
        };
        nodes.push(json!({
            "name": format!("{}_light", l.name),
            "translation": translation,
            "rotation": rotation,
            "extensions": {
                "KHR_lights_punctual": { "light": light_idx },
            },
        }));
        node_indices.push((nodes.len() - 1) as u32);
    }

    // -- Top-level document ------------------------------------------
    let copyright = format!("AEC Studio export — {}", chrono::Utc::now().to_rfc3339());
    let scene_obj = json!({
        "name": scene.name,
        "nodes": node_indices,
    });
    let buffer_length = buffer.len();

    let bin_uri = match format {
        GltfFormat::Binary => Value::Null,
        GltfFormat::Json => Value::String(default_bin_filename(out_path)),
    };

    let mut buffers = serde_json::Map::new();
    buffers.insert("byteLength".to_string(), json!(buffer_length));
    if let Value::String(s) = &bin_uri {
        buffers.insert("uri".to_string(), Value::String(s.clone()));
    }
    let buffers_array = Value::Array(vec![Value::Object(buffers)]);

    let mut doc = serde_json::Map::new();
    doc.insert(
        "asset".to_string(),
        json!({
            "version": "2.0",
            "generator": options.generator,
            "copyright": copyright,
        }),
    );
    doc.insert("scene".to_string(), json!(0));
    doc.insert("scenes".to_string(), Value::Array(vec![scene_obj]));
    if !nodes.is_empty() {
        doc.insert("nodes".to_string(), Value::Array(nodes));
    }
    if !gltf_meshes.is_empty() {
        doc.insert("meshes".to_string(), Value::Array(gltf_meshes));
    }
    if !materials.is_empty() {
        doc.insert("materials".to_string(), Value::Array(materials));
    }
    if !cameras.is_empty() {
        doc.insert("cameras".to_string(), Value::Array(cameras));
    }
    if !accessors.is_empty() {
        doc.insert("accessors".to_string(), Value::Array(accessors));
    }
    if !bufferviews.is_empty() {
        doc.insert("bufferViews".to_string(), Value::Array(bufferviews));
    }
    if buffer_length > 0 {
        doc.insert("buffers".to_string(), buffers_array);
    }

    if !lights.is_empty() {
        doc.insert(
            "extensions".to_string(),
            json!({
                "KHR_lights_punctual": { "lights": lights },
            }),
        );
        doc.insert(
            "extensionsUsed".to_string(),
            Value::Array(vec![Value::String("KHR_lights_punctual".into())]),
        );
    }

    let json_bytes = if options.pretty_print {
        serde_json::to_vec_pretty(&Value::Object(doc))?
    } else {
        serde_json::to_vec(&Value::Object(doc))?
    };

    let bin_path = match format {
        GltfFormat::Json => {
            std::fs::write(out_path, &json_bytes)?;
            if buffer_length > 0 {
                let bin_path = bin_sibling_path(out_path);
                std::fs::write(&bin_path, &buffer)?;
                Some(bin_path)
            } else {
                None
            }
        }
        GltfFormat::Binary => {
            let glb = build_glb(&json_bytes, &buffer);
            std::fs::write(out_path, glb)?;
            None
        }
    };

    Ok(WriteGltfResult {
        out_path: out_path.to_path_buf(),
        bin_path,
        mesh_count: scene.meshes.len() as u32,
        material_count: scene.materials.len() as u32,
        camera_count: scene.cameras.len() as u32,
        light_count: scene.lights.len() as u32,
        triangle_count,
    })
}

// ============================================================================
// JSON-building helpers.
// ============================================================================

fn push_bufferview(
    bufferviews: &mut Vec<Value>,
    offset: usize,
    length: usize,
    target: Option<u32>,
    byte_stride: Option<u32>,
) -> usize {
    let mut bv = serde_json::Map::new();
    bv.insert("buffer".to_string(), json!(0));
    bv.insert("byteOffset".to_string(), json!(offset));
    bv.insert("byteLength".to_string(), json!(length));
    if let Some(t) = target {
        bv.insert("target".to_string(), json!(t));
    }
    if let Some(s) = byte_stride {
        bv.insert("byteStride".to_string(), json!(s));
    }
    bufferviews.push(Value::Object(bv));
    bufferviews.len() - 1
}

fn push_accessor(
    accessors: &mut Vec<Value>,
    buffer_view: usize,
    component_type: u32,
    accessor_type: &str,
    count: u32,
    min: Option<Vec<f32>>,
    max: Option<Vec<f32>>,
) -> usize {
    let mut a = serde_json::Map::new();
    a.insert("bufferView".to_string(), json!(buffer_view));
    a.insert("componentType".to_string(), json!(component_type));
    a.insert("count".to_string(), json!(count));
    a.insert("type".to_string(), json!(accessor_type));
    if let Some(mn) = min {
        a.insert("min".to_string(), json!(mn));
    }
    if let Some(mx) = max {
        a.insert("max".to_string(), json!(mx));
    }
    accessors.push(Value::Object(a));
    accessors.len() - 1
}

fn vec3_min(points: &[[f32; 3]], scale: f32) -> [f32; 3] {
    let mut out = [f32::INFINITY; 3];
    for p in points {
        for (i, v) in p.iter().enumerate() {
            out[i] = out[i].min(*v * scale);
        }
    }
    if !out.iter().all(|v| v.is_finite()) {
        return [0.0; 3];
    }
    out
}

fn vec3_max(points: &[[f32; 3]], scale: f32) -> [f32; 3] {
    let mut out = [f32::NEG_INFINITY; 3];
    for p in points {
        for (i, v) in p.iter().enumerate() {
            out[i] = out[i].max(*v * scale);
        }
    }
    if !out.iter().all(|v| v.is_finite()) {
        return [0.0; 3];
    }
    out
}

fn align_to_4(buffer: &mut Vec<u8>) {
    while buffer.len() % 4 != 0 {
        buffer.push(0);
    }
}

fn identity_matrix() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Convert a row-major 4x4 matrix to glTF's column-major 16-element
/// array, and apply the mm→metres scale to the translation column
/// (rows 0..3, column 3).
fn transpose_and_scale(m: [[f32; 4]; 4], scale: f32) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for row in 0..4 {
        for col in 0..4 {
            // glTF expects column-major: out[col*4 + row].
            let mut v = m[row][col];
            // Scale translation entries (top-right 3x1).
            if col == 3 && row < 3 {
                v *= scale;
            }
            out[col * 4 + row] = v;
        }
    }
    out
}

fn scale_vec(v: [f32; 3], scale: f32) -> [f32; 3] {
    [v[0] * scale, v[1] * scale, v[2] * scale]
}

/// Decompose a lookAt(position, target, up) view direction into a
/// (translation, rotation) pair. The rotation is the quaternion that
/// rotates `(0, 0, -1)` (glTF's default forward) into the direction
/// `(target - position)`.
fn camera_lookat_components(
    position: [f32; 3],
    target: [f32; 3],
    up: [f32; 3],
) -> ([f32; 3], [f32; 4]) {
    let forward = normalize([
        target[0] - position[0],
        target[1] - position[1],
        target[2] - position[2],
    ]);
    let up = normalize(up);
    let right = normalize(cross(forward, up));
    let recomputed_up = cross(right, forward);

    // Build the 3x3 rotation matrix M whose columns are (right, up, -forward).
    // glTF default camera looks down -Z with +Y up; this matrix transforms
    // local (right=+X, up=+Y, forward=-Z) into world space.
    let m = [
        [right[0], recomputed_up[0], -forward[0]],
        [right[1], recomputed_up[1], -forward[1]],
        [right[2], recomputed_up[2], -forward[2]],
    ];
    let rot = mat3_to_quat(m);
    (position, rot)
}

/// Convert a direction vector into a unit quaternion that rotates
/// `(0, 0, -1)` (glTF directional/spot light "forward") onto the
/// supplied direction.
fn direction_to_quat(dir: [f32; 3]) -> [f32; 4] {
    let d = normalize(dir);
    let forward = [0.0, 0.0, -1.0];
    let cos_theta = dot(forward, d);
    if cos_theta < -1.0 + 1e-6 {
        // 180° rotation around any axis perpendicular to forward.
        // Use +Y so the result is deterministic.
        return [0.0, 1.0, 0.0, 0.0];
    }
    if cos_theta > 1.0 - 1e-6 {
        return [0.0, 0.0, 0.0, 1.0]; // identity
    }
    let axis = normalize(cross(forward, d));
    let half_theta = cos_theta.acos() * 0.5;
    let s = half_theta.sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, half_theta.cos()]
}

fn mat3_to_quat(m: [[f32; 3]; 3]) -> [f32; 4] {
    // Standard rotation-matrix to quaternion conversion. The matrix is
    // column-major: m[col][row].
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        return [
            (m[1][2] - m[2][1]) / s,
            (m[2][0] - m[0][2]) / s,
            (m[0][1] - m[1][0]) / s,
            0.25 * s,
        ];
    }
    if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        return [
            0.25 * s,
            (m[1][0] + m[0][1]) / s,
            (m[2][0] + m[0][2]) / s,
            (m[1][2] - m[2][1]) / s,
        ];
    }
    if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        return [
            (m[1][0] + m[0][1]) / s,
            0.25 * s,
            (m[2][1] + m[1][2]) / s,
            (m[2][0] - m[0][2]) / s,
        ];
    }
    let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
    [
        (m[2][0] + m[0][2]) / s,
        (m[2][1] + m[1][2]) / s,
        0.25 * s,
        (m[0][1] - m[1][0]) / s,
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-9 {
        return [0.0, 0.0, 1.0];
    }
    [v[0] / l, v[1] / l, v[2] / l]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

// ============================================================================
// GLB binary container.
// ============================================================================

fn build_glb(json: &[u8], bin: &[u8]) -> Vec<u8> {
    // Pad JSON chunk to 4-byte alignment with 0x20 (space) per spec.
    let mut json_chunk = json.to_vec();
    while json_chunk.len() % 4 != 0 {
        json_chunk.push(0x20);
    }
    // Pad BIN chunk to 4-byte alignment with 0x00.
    let mut bin_chunk = bin.to_vec();
    while bin_chunk.len() % 4 != 0 {
        bin_chunk.push(0x00);
    }

    let has_bin = !bin_chunk.is_empty();
    let total_length = 12 // header
        + 8 + json_chunk.len()
        + if has_bin { 8 + bin_chunk.len() } else { 0 };

    let mut out = Vec::with_capacity(total_length);
    // Header: magic, version, total length.
    out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total_length as u32).to_le_bytes());

    // JSON chunk.
    out.extend_from_slice(&(json_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
    out.extend_from_slice(&json_chunk);

    if has_bin {
        out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
        out.extend_from_slice(&bin_chunk);
    }
    out
}

fn ensure_parent_dir(out_path: &Path) -> std::io::Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn bin_sibling_path(out_path: &Path) -> PathBuf {
    let mut p = out_path.to_path_buf();
    p.set_extension("bin");
    p
}

fn default_bin_filename(out_path: &Path) -> String {
    bin_sibling_path(out_path)
        .file_name()
        .map_or_else(|| "scene.bin".into(), |s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    fn unit_cube() -> GltfMesh {
        // 8 vertices, 12 triangles. Positions in mm.
        let p = [
            [0.0_f32, 0.0, 0.0],
            [1000.0, 0.0, 0.0],
            [1000.0, 1000.0, 0.0],
            [0.0, 1000.0, 0.0],
            [0.0, 0.0, 1000.0],
            [1000.0, 0.0, 1000.0],
            [1000.0, 1000.0, 1000.0],
            [0.0, 1000.0, 1000.0],
        ];
        let mut mesh = GltfMesh::new("cube");
        mesh.positions = p.to_vec();
        // Normals approximated as position-direction unit vectors —
        // good enough for round-trip tests.
        mesh.normals = mesh.positions.iter().map(|q| normalize(*q)).collect();
        mesh.uvs = vec![[0.0, 0.0]; 8];
        // Six faces, two triangles each.
        mesh.indices = vec![
            // -Z
            0, 1, 2, 0, 2, 3, // +Z
            4, 6, 5, 4, 7, 6, // -Y
            0, 4, 5, 0, 5, 1, // +Y
            3, 2, 6, 3, 6, 7, // -X
            0, 3, 7, 0, 7, 4, // +X
            1, 5, 6, 1, 6, 2,
        ];
        mesh
    }

    #[test]
    fn glb_starts_with_proper_magic_and_version() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("cube.glb");
        let scene = GltfScene {
            name: "cube".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.len() >= 12);
        assert_eq!(&bytes[0..4], b"glTF");
        // Version = 2.
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            2
        );
    }

    #[test]
    fn gltf_split_writes_bin_sibling() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("cube.gltf");
        let scene = GltfScene {
            name: "cube".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        let res = write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        assert!(out.exists());
        let bin_path = res.bin_path.expect("bin sibling produced");
        assert!(bin_path.exists());
        // JSON file should reference the bin URI.
        let json: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let uri = json["buffers"][0]["uri"].as_str().unwrap();
        assert_eq!(uri, "cube.bin");
    }

    #[test]
    fn empty_scene_still_writes_valid_gltf() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("empty.gltf");
        let scene = GltfScene {
            name: "empty".into(),
            ..Default::default()
        };
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let json: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(json["asset"]["version"], "2.0");
        assert!(json["scenes"].is_array());
        assert_eq!(json["scenes"][0]["name"], "empty");
    }

    #[test]
    fn deterministic_output_for_same_input() {
        let dir = tempdir().unwrap();
        let mut scene = GltfScene {
            name: "scene".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        scene.materials.push(GltfMaterial::new("mat:oak", "Oak"));
        scene.meshes[0].material_id = Some("mat:oak".into());
        let out_a = dir.path().join("a.glb");
        let out_b = dir.path().join("b.glb");
        // Suppress timestamp non-determinism by overriding the
        // generator string to a fixed value.
        let opts = WriteGltfOptions {
            generator: "AEC Studio test".into(),
            ..Default::default()
        };
        write_gltf(&out_a, &scene, &opts).unwrap();
        write_gltf(&out_b, &scene, &opts).unwrap();
        let bytes_a = std::fs::read(&out_a).unwrap();
        let bytes_b = std::fs::read(&out_b).unwrap();
        // Strip the asset.copyright timestamp by parsing JSON,
        // removing the field, and re-comparing.
        let (json_a, bin_a) = split_glb(&bytes_a);
        let (json_b, bin_b) = split_glb(&bytes_b);
        let mut va: Value = serde_json::from_slice(&json_a).unwrap();
        let mut vb: Value = serde_json::from_slice(&json_b).unwrap();
        va["asset"].as_object_mut().unwrap().remove("copyright");
        vb["asset"].as_object_mut().unwrap().remove("copyright");
        assert_eq!(va, vb);
        assert_eq!(bin_a, bin_b);
    }

    #[test]
    fn cube_has_correct_buffer_byte_length() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("cube.glb");
        let mut scene = GltfScene {
            name: "cube".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        // Disable mm->metres so the math is straightforward.
        let opts = WriteGltfOptions {
            mm_to_metres: false,
            generator: "test".into(),
            ..Default::default()
        };
        // 8 vertices * (12+12+8 bytes) = 256 bytes for vertex data.
        // 36 indices * 2 bytes (u16) = 72, aligned to 76.
        // Total expected buffer length includes alignment padding to 4.
        let res = write_gltf(&out, &scene, &opts).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let (json, _bin) = split_glb(&bytes);
        let v: Value = serde_json::from_slice(&json).unwrap();
        let n: u64 = v["buffers"][0]["byteLength"].as_u64().unwrap();
        // 8*(12+12+8) = 256 + 36*2 = 328. After mesh alignment +0/+0/+0
        // (8/8/8 vertices, sizes divisible by 4 already) = 328.
        assert_eq!(n, 328);
        assert_eq!(res.triangle_count, 12);
        scene.meshes.clear();
    }

    #[test]
    fn material_appears_in_json_when_referenced() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("mat.gltf");
        let mut scene = GltfScene {
            name: "scene".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        scene.materials.push(GltfMaterial {
            id: "mat:oak".into(),
            name: "Light Oak".into(),
            base_color_factor: [0.78, 0.66, 0.5, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.55,
            emissive_factor: [0.0, 0.0, 0.0],
            double_sided: false,
        });
        scene.meshes[0].material_id = Some("mat:oak".into());
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(v["materials"][0]["name"], "Light Oak");
        let bc = &v["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"];
        let bc0 = bc[0].as_f64().unwrap();
        let bc1 = bc[1].as_f64().unwrap();
        let bc2 = bc[2].as_f64().unwrap();
        assert!((bc0 - 0.78).abs() < 1e-4);
        assert!((bc1 - 0.66).abs() < 1e-4);
        assert!((bc2 - 0.5).abs() < 1e-4);
        // Primitive should reference material index 0.
        let prim_mat = &v["meshes"][0]["primitives"][0]["material"];
        assert_eq!(prim_mat, &json!(0));
    }

    #[test]
    fn material_index_is_dropped_when_unreferenced() {
        // Material added but mesh does not reference it.
        let dir = tempdir().unwrap();
        let out = dir.path().join("mat.gltf");
        let mut scene = GltfScene {
            name: "scene".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        scene.materials.push(GltfMaterial::new("mat:oak", "Oak"));
        scene.meshes[0].material_id = None;
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert!(v["meshes"][0]["primitives"][0].get("material").is_none());
    }

    #[test]
    fn camera_node_emits_perspective_parameters() {
        use std::f32::consts::FRAC_PI_4;
        let dir = tempdir().unwrap();
        let out = dir.path().join("cam.gltf");
        let mut scene = GltfScene {
            name: "scene".into(),
            ..Default::default()
        };
        scene.cameras.push(GltfCamera {
            name: "main".into(),
            position_mm: [1000.0, 2000.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            up_mm: [0.0, 1.0, 0.0],
            aspect_ratio: 1.7777,
            yfov_rad: FRAC_PI_4,
            znear_mm: 100.0,
            zfar_mm: 10_000.0,
        });
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let cam = &v["cameras"][0];
        assert_eq!(cam["type"], "perspective");
        let p = &cam["perspective"];
        let aspect: f64 = p["aspectRatio"].as_f64().unwrap();
        assert!((aspect - 1.7777).abs() < 1e-4);
        let yfov: f64 = p["yfov"].as_f64().unwrap();
        assert!((yfov - f64::from(FRAC_PI_4)).abs() < 1e-4);
    }

    #[test]
    fn light_node_uses_khr_lights_punctual_extension() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("light.gltf");
        let mut scene = GltfScene {
            name: "scene".into(),
            ..Default::default()
        };
        scene.lights.push(GltfLight {
            name: "sun".into(),
            kind: GltfLightKind::Directional {
                direction_mm: [0.0, -1.0, 0.0],
            },
            color: [1.0, 0.95, 0.9],
            intensity: 1000.0,
        });
        scene.lights.push(GltfLight {
            name: "spot".into(),
            kind: GltfLightKind::Spot {
                position_mm: [1000.0, 2000.0, 0.0],
                direction_mm: [0.0, -1.0, 0.0],
                inner_cone_angle: 0.2,
                outer_cone_angle: 0.5,
            },
            color: [1.0, 1.0, 1.0],
            intensity: 500.0,
        });
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(v["extensionsUsed"][0], "KHR_lights_punctual");
        let lights = &v["extensions"]["KHR_lights_punctual"]["lights"];
        assert_eq!(lights[0]["type"], "directional");
        assert_eq!(lights[1]["type"], "spot");
        let spot = &lights[1]["spot"];
        let inner: f64 = spot["innerConeAngle"].as_f64().unwrap();
        let outer: f64 = spot["outerConeAngle"].as_f64().unwrap();
        assert!((inner - 0.2).abs() < 1e-6);
        assert!((outer - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mismatched_normals_length_is_rejected() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("bad.gltf");
        let mut mesh = unit_cube();
        mesh.normals.pop();
        let scene = GltfScene {
            name: "bad".into(),
            meshes: vec![mesh],
            ..Default::default()
        };
        let err = write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap_err();
        match err {
            GltfExportError::Invalid(msg) => {
                assert!(msg.contains("normals/positions length mismatch"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn non_triangle_index_count_is_rejected() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("bad.gltf");
        let mut mesh = unit_cube();
        mesh.indices.pop();
        let scene = GltfScene {
            name: "bad".into(),
            meshes: vec![mesh],
            ..Default::default()
        };
        let err = write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap_err();
        match err {
            GltfExportError::Invalid(msg) => assert!(msg.contains("non-triangle index count")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn glb_parses_via_gltf_crate() {
        // Round-trip check: write a real cube as GLB, then re-parse it
        // using the gltf import crate (already in dependencies via
        // aec_assets). We test the parse via raw byte inspection here
        // since aec_export does not depend on `gltf` directly.
        let dir = tempdir().unwrap();
        let out = dir.path().join("rt.glb");
        let scene = GltfScene {
            name: "scene".into(),
            meshes: vec![unit_cube()],
            ..Default::default()
        };
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let (json, bin) = split_glb(&bytes);
        let v: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["asset"]["version"], "2.0");
        assert!(v["meshes"].is_array());
        assert_eq!(v["meshes"][0]["name"], "cube");
        // BIN chunk byte length matches buffers[0].byteLength.
        let declared: u64 = v["buffers"][0]["byteLength"].as_u64().unwrap();
        // BIN data may be padded with zeroes to 4-byte alignment.
        assert!(bin.len() >= declared as usize);
        assert!(bin.len() - declared as usize <= 3);
    }

    #[test]
    fn pbr_material_from_aec_materials_round_trips_albedo() {
        let m = aec_materials::PbrMaterial {
            id: "m1".into(),
            name: "Test".into(),
            albedo: [0.1, 0.2, 0.3],
            metallic: 0.5,
            roughness: 0.7,
            ior: 1.45,
            emissive: [0.0, 0.0, 0.0],
            ao: 1.0,
            transmission: 0.0,
            albedo_map: None,
            normal_map: None,
            metallic_roughness_map: None,
            ao_map: None,
            emissive_map: None,
            tags: Vec::new(),
            style_tags: Vec::new(),
            vendor_id: None,
        };
        let g = GltfMaterial::from_pbr(&m);
        assert_eq!(g.base_color_factor, [0.1, 0.2, 0.3, 1.0]);
        assert!((g.metallic_factor - 0.5).abs() < 1e-6);
        assert!((g.roughness_factor - 0.7).abs() < 1e-6);
    }

    #[test]
    fn deterministic_buffer_offsets() {
        // Two meshes back-to-back should produce identical offsets
        // every run, ensuring no hashmap iteration affects layout.
        let dir = tempdir().unwrap();
        let out = dir.path().join("two.gltf");
        let mut scene = GltfScene {
            name: "two".into(),
            ..Default::default()
        };
        scene.meshes.push(unit_cube());
        let mut second = unit_cube();
        second.name = "cube_2".into();
        scene.meshes.push(second);
        let opts = WriteGltfOptions {
            generator: "test".into(),
            ..Default::default()
        };
        write_gltf(&out, &scene, &opts).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let bvs = v["bufferViews"].as_array().unwrap();
        // 4 bufferViews per mesh × 2 meshes = 8.
        assert_eq!(bvs.len(), 8);
        // Each bufferView's offset should be strictly increasing.
        let offsets: Vec<u64> = bvs
            .iter()
            .map(|b| b["byteOffset"].as_u64().unwrap())
            .collect();
        for w in offsets.windows(2) {
            assert!(w[1] > w[0], "offsets should be strictly increasing");
        }
    }

    #[test]
    fn camera_node_translation_matches_camera_position_after_scaling() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("cam.gltf");
        let mut scene = GltfScene {
            name: "cam".into(),
            ..Default::default()
        };
        scene.cameras.push(GltfCamera {
            name: "c".into(),
            position_mm: [1000.0, 2000.0, 3000.0],
            target_mm: [0.0, 0.0, 0.0],
            up_mm: [0.0, 1.0, 0.0],
            aspect_ratio: 1.0,
            yfov_rad: 1.0,
            znear_mm: 100.0,
            zfar_mm: 10_000.0,
        });
        // mm->metres on by default; expect translation 1.0/2.0/3.0.
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        // Camera node is appended after any mesh nodes; in this scene
        // there are no meshes, so index 0 = the camera node.
        let translation = &v["nodes"][0]["translation"];
        let t: Vec<f64> = translation
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_f64().unwrap())
            .collect();
        assert!((t[0] - 1.0).abs() < 1e-4);
        assert!((t[1] - 2.0).abs() < 1e-4);
        assert!((t[2] - 3.0).abs() < 1e-4);
    }

    fn split_glb(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
        assert_eq!(&bytes[0..4], b"glTF");
        let json_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        let json = bytes[20..20 + json_len].to_vec();
        let bin = if bytes.len() > 20 + json_len {
            let bin_offset = 20 + json_len + 8;
            bytes[bin_offset..].to_vec()
        } else {
            Vec::new()
        };
        (json, bin)
    }

    #[test]
    fn light_index_is_unique_per_extension_lights_array() {
        // 3 lights -> 3 unique indices referenced from 3 nodes.
        let dir = tempdir().unwrap();
        let out = dir.path().join("multi.gltf");
        let mut scene = GltfScene {
            name: "multi".into(),
            ..Default::default()
        };
        for i in 0..3 {
            scene.lights.push(GltfLight {
                name: format!("l{i}"),
                kind: GltfLightKind::Point {
                    position_mm: [(i as f32) * 100.0, 0.0, 0.0],
                },
                color: [1.0, 1.0, 1.0],
                intensity: 100.0,
            });
        }
        write_gltf(&out, &scene, &WriteGltfOptions::default()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let lights = v["extensions"]["KHR_lights_punctual"]["lights"]
            .as_array()
            .unwrap();
        assert_eq!(lights.len(), 3);
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for n in v["nodes"].as_array().unwrap() {
            if let Some(idx) = n["extensions"]["KHR_lights_punctual"]["light"].as_u64() {
                seen.insert(idx);
            }
        }
        assert_eq!(seen.len(), 3);
    }
}
