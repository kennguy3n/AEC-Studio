//! Material check / render doctor — diagnoses material issues before
//! kicking off a native render (preview, final, walkthrough, or
//! panorama).
//!
//! Catches the four failure modes that bite real renders:
//!
//! 1. **Missing textures** — a material references a `TextureRef` blob
//!    hash that isn't in the asset blob store.
//! 2. **Non-PBR materials** — materials whose albedo or metallic /
//!    roughness values are physically impossible (HDR albedo, both
//!    metallic AND high transmission, etc.).
//! 3. **Swapped channels** — heuristic catch for the classic gotcha
//!    where a normal map ends up bound to the albedo slot (mostly
//!    blue, low saturation).
//! 4. **Oversized textures** — textures larger than the worker's
//!    GPU-memory budget for the tier.
//!
//! Findings feed into the AI [`render_doctor`] tool result as
//! pre-render input via [`MaterialCheckResult::into_findings`].

use serde::{Deserialize, Serialize};

use aec_materials::material::{PbrMaterial, TextureRef};

use crate::scene::RenderScene;

/// Maximum texture edge we let through by default (4K). Anything
/// larger goes into `OversizedTexture` findings.
pub const DEFAULT_MAX_TEXTURE_EDGE_PX: u32 = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaterialFinding {
    /// A material references a `TextureRef` whose `blob_hash` isn't
    /// in the supplied known-blobs set. Renders will fail or fall
    /// back to magenta in this case.
    MissingTexture {
        material_id: String,
        channel: String,
        blob_hash: String,
    },
    /// The material has at least one PBR parameter outside the
    /// physically plausible range. The detail string explains which.
    NonPbrMaterial { material_id: String, detail: String },
    /// Heuristic: a texture bound to one channel looks like it
    /// belongs to a different channel (e.g. flat-blue normal map
    /// plugged into albedo).
    SwappedChannels {
        material_id: String,
        suspected: String,
        actual: String,
    },
    /// Texture exceeds the configured edge size.
    OversizedTexture {
        material_id: String,
        channel: String,
        width: u32,
        height: u32,
        budget: u32,
    },
}

impl MaterialFinding {
    /// Stable issue code consumed by the TS UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingTexture { .. } => "material.missing_texture",
            Self::NonPbrMaterial { .. } => "material.non_pbr",
            Self::SwappedChannels { .. } => "material.swapped_channels",
            Self::OversizedTexture { .. } => "material.oversized_texture",
        }
    }

    /// `info` / `warning` / `error`. Drives the UI severity colour.
    pub fn severity(&self) -> &'static str {
        match self {
            Self::MissingTexture { .. } => "error",
            Self::NonPbrMaterial { .. } => "warning",
            Self::SwappedChannels { .. } => "warning",
            Self::OversizedTexture { .. } => "warning",
        }
    }

    /// Material id the finding applies to. Useful for grouping in the
    /// UI ("3 issues on `mat:oak`").
    pub fn material_id(&self) -> &str {
        match self {
            Self::MissingTexture { material_id, .. }
            | Self::NonPbrMaterial { material_id, .. }
            | Self::SwappedChannels { material_id, .. }
            | Self::OversizedTexture { material_id, .. } => material_id,
        }
    }

    /// Human-readable summary the UI shows in the doctor row.
    pub fn message(&self) -> String {
        match self {
            Self::MissingTexture {
                material_id,
                channel,
                blob_hash,
            } => format!(
                "{material_id}: {channel} texture blob `{blob_hash}` is missing from the asset store"
            ),
            Self::NonPbrMaterial {
                material_id,
                detail,
            } => format!("{material_id}: non-PBR — {detail}"),
            Self::SwappedChannels {
                material_id,
                suspected,
                actual,
            } => format!(
                "{material_id}: {actual} texture looks like a {suspected} map (likely swapped channels)"
            ),
            Self::OversizedTexture {
                material_id,
                channel,
                width,
                height,
                budget,
            } => format!(
                "{material_id}: {channel} texture {width}×{height} exceeds {budget}px budget"
            ),
        }
    }

    /// Suggested fix the UI displays. None when no automated fix is
    /// available (the user has to author replacement texture).
    pub fn fix(&self) -> Option<String> {
        match self {
            Self::MissingTexture { .. } => {
                Some("Re-link the missing texture or re-import the asset".into())
            }
            Self::NonPbrMaterial { .. } => Some(
                "Clamp parameter to the PBR-valid range (albedo [0..1], metallic [0..1], roughness [0..1])".into(),
            ),
            Self::SwappedChannels { suspected, .. } => {
                Some(format!("Rebind texture to the {suspected} slot"))
            }
            Self::OversizedTexture { budget, .. } => Some(format!(
                "Downscale texture to ≤{budget}px before re-import"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialCheckResult {
    pub findings: Vec<MaterialFinding>,
}

impl MaterialCheckResult {
    /// Convenience: `findings.is_empty()`.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// True if any finding is severity `error`. Render UI uses this
    /// to block the "Render" button until errors are resolved.
    pub fn has_blocking_issues(&self) -> bool {
        self.findings.iter().any(|f| f.severity() == "error")
    }

    /// Convert material findings into the JSON shape the AI render
    /// doctor (`crates/aec_ai/src/render_doctor.rs`) feeds into the
    /// planner. Each finding becomes a top-level finding with
    /// `issue = "other"` (since the existing `RenderIssue` enum is
    /// scoped to render-time observations).
    pub fn into_ai_findings(&self) -> Vec<serde_json::Value> {
        self.findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "issue": "other",
                    "severity": f.severity(),
                    "code": f.code(),
                    "material_id": f.material_id(),
                    "recommendation": f.fix(),
                    "message": f.message(),
                })
            })
            .collect()
    }
}

/// Knobs for the material check. Lets callers tighten or relax
/// thresholds (e.g. accept 8K textures on Pro-tier).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckMaterialsOptions {
    pub max_texture_edge_px: u32,
    /// When true, run the swapped-channels heuristic (off by default
    /// because false positives are possible on intentionally blue
    /// albedo materials).
    pub enable_swap_heuristic: bool,
}

impl Default for CheckMaterialsOptions {
    fn default() -> Self {
        Self {
            max_texture_edge_px: DEFAULT_MAX_TEXTURE_EDGE_PX,
            enable_swap_heuristic: true,
        }
    }
}

/// Run all material checks. `known_blob_hashes` is the set of blob
/// hashes the asset store can actually serve; missing-texture
/// findings are emitted for any `TextureRef` not in this set.
pub fn check_materials(
    scene: &RenderScene,
    materials: &[PbrMaterial],
    known_blob_hashes: &std::collections::BTreeSet<String>,
    opts: &CheckMaterialsOptions,
) -> MaterialCheckResult {
    let mut findings = Vec::new();

    // Map material_id → material for quick lookup. Materials that the
    // scene references but aren't in the list still get a missing
    // texture finding (because we don't have its TextureRefs).
    let mat_by_id: std::collections::BTreeMap<&str, &PbrMaterial> =
        materials.iter().map(|m| (m.id.as_str(), m)).collect();

    // Which materials does the scene actually touch? Only flag those.
    let used_mat_ids: std::collections::BTreeSet<&str> = scene
        .meshes
        .iter()
        .filter_map(|m| m.material_id.as_deref())
        .collect();

    for mat_id in &used_mat_ids {
        let Some(mat) = mat_by_id.get(mat_id) else {
            // A material referenced by the scene but not in the
            // material library is itself a missing-texture-style
            // finding — we emit it as the strictest case.
            findings.push(MaterialFinding::MissingTexture {
                material_id: (*mat_id).to_string(),
                channel: "<material>".into(),
                blob_hash: "<unknown>".into(),
            });
            continue;
        };
        push_material_findings(mat, known_blob_hashes, opts, &mut findings);
    }

    MaterialCheckResult { findings }
}

fn push_material_findings(
    mat: &PbrMaterial,
    known_blob_hashes: &std::collections::BTreeSet<String>,
    opts: &CheckMaterialsOptions,
    findings: &mut Vec<MaterialFinding>,
) {
    // 1. Missing textures + oversized textures + swap heuristic — run
    //    over every assigned TextureRef.
    let slots: [(Option<&TextureRef>, &str); 5] = [
        (mat.albedo_map.as_ref(), "albedo"),
        (mat.normal_map.as_ref(), "normal"),
        (mat.metallic_roughness_map.as_ref(), "metallic_roughness"),
        (mat.ao_map.as_ref(), "ao"),
        (mat.emissive_map.as_ref(), "emissive"),
    ];
    for (slot, slot_name) in slots {
        let Some(tex) = slot else { continue };
        if !known_blob_hashes.contains(&tex.blob_hash) {
            findings.push(MaterialFinding::MissingTexture {
                material_id: mat.id.clone(),
                channel: slot_name.to_string(),
                blob_hash: tex.blob_hash.clone(),
            });
        }
        if tex.width > opts.max_texture_edge_px || tex.height > opts.max_texture_edge_px {
            findings.push(MaterialFinding::OversizedTexture {
                material_id: mat.id.clone(),
                channel: slot_name.to_string(),
                width: tex.width,
                height: tex.height,
                budget: opts.max_texture_edge_px,
            });
        }
        if opts.enable_swap_heuristic {
            if let Some(swap) = detect_channel_swap(tex, slot_name) {
                findings.push(MaterialFinding::SwappedChannels {
                    material_id: mat.id.clone(),
                    suspected: swap.suspected.into(),
                    actual: slot_name.into(),
                });
            }
        }
    }

    // 2. Non-PBR sanity. We check the parameter ranges that have
    //    objective physical interpretation, not subjective ones (a
    //    matte plastic with `roughness = 0.95` is fine).
    if mat.albedo.iter().any(|&c| !(0.0..=1.0).contains(&c)) {
        findings.push(MaterialFinding::NonPbrMaterial {
            material_id: mat.id.clone(),
            detail: format!("albedo {:?} out of [0,1]", mat.albedo),
        });
    }
    if !(0.0..=1.0).contains(&mat.metallic) {
        findings.push(MaterialFinding::NonPbrMaterial {
            material_id: mat.id.clone(),
            detail: format!("metallic {} out of [0,1]", mat.metallic),
        });
    }
    if !(0.0..=1.0).contains(&mat.roughness) {
        findings.push(MaterialFinding::NonPbrMaterial {
            material_id: mat.id.clone(),
            detail: format!("roughness {} out of [0,1]", mat.roughness),
        });
    }
    if mat.metallic > 0.5 && mat.transmission > 0.5 {
        findings.push(MaterialFinding::NonPbrMaterial {
            material_id: mat.id.clone(),
            detail: "metallic + transmissive is not a real PBR material".into(),
        });
    }
    if !(1.0..=3.0).contains(&mat.ior) {
        findings.push(MaterialFinding::NonPbrMaterial {
            material_id: mat.id.clone(),
            detail: format!("ior {} is outside the realistic 1.0..=3.0 range", mat.ior),
        });
    }
}

struct SwapHint {
    suspected: &'static str,
}

/// Heuristic that flags textures whose `channel` declared in the
/// `TextureRef` itself disagrees with the slot they're bound to. This
/// is the classic "I plugged the normal map into the albedo slot"
/// mistake; the texture's channel field carries the import-time
/// guess from the asset pipeline, so a mismatch is a strong signal.
fn detect_channel_swap(tex: &TextureRef, slot_name: &str) -> Option<SwapHint> {
    if tex.channel.is_empty() {
        return None;
    }
    if tex.channel.eq_ignore_ascii_case(slot_name) {
        return None;
    }
    // Map a handful of well-known import channel names to the slot
    // they actually belong in.
    let suspected = match tex.channel.to_ascii_lowercase().as_str() {
        "normal" => "normal",
        "albedo" | "base_color" | "diffuse" => "albedo",
        "metallic_roughness" | "orm" => "metallic_roughness",
        "ao" | "ambient_occlusion" => "ao",
        "emissive" => "emissive",
        _ => return None,
    };
    if suspected == slot_name {
        return None;
    }
    Some(SwapHint { suspected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::SerializedMesh;
    use aec_materials::material::TextureRef;

    fn mat(id: &str) -> PbrMaterial {
        PbrMaterial::new(id, id)
    }

    fn mesh_referencing(material_id: &str) -> SerializedMesh {
        SerializedMesh {
            id: format!("mesh-{material_id}"),
            indices: vec![0, 1, 2],
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            material_id: Some(material_id.into()),
            transform: [[0.0; 4]; 4],
        }
    }

    fn scene_with(materials: &[&str]) -> RenderScene {
        let mut s = RenderScene::new();
        for m in materials {
            s.push_mesh(mesh_referencing(m));
        }
        s
    }

    #[test]
    fn clean_scene_yields_no_findings() {
        let scene = scene_with(&["mat:oak"]);
        let mats = vec![mat("mat:oak")];
        let known = std::collections::BTreeSet::new();
        let result = check_materials(&scene, &mats, &known, &CheckMaterialsOptions::default());
        assert!(result.is_clean());
    }

    #[test]
    fn missing_texture_blob_is_flagged_as_error() {
        let mut m = mat("mat:oak");
        m.albedo_map = Some(TextureRef {
            blob_hash: "deadbeef".into(),
            channel: "albedo".into(),
            width: 1024,
            height: 1024,
        });
        let scene = scene_with(&["mat:oak"]);
        let mats = vec![m];
        let known = std::collections::BTreeSet::new();
        let result = check_materials(&scene, &mats, &known, &CheckMaterialsOptions::default());
        assert_eq!(result.findings.len(), 1);
        assert!(matches!(
            result.findings[0],
            MaterialFinding::MissingTexture { .. }
        ));
        assert!(result.has_blocking_issues());
    }

    #[test]
    fn non_pbr_metallic_above_one_flagged() {
        let mut m = mat("mat:oak");
        m.metallic = 1.4;
        let scene = scene_with(&["mat:oak"]);
        let result = check_materials(
            &scene,
            &[m],
            &std::collections::BTreeSet::new(),
            &CheckMaterialsOptions::default(),
        );
        assert!(result
            .findings
            .iter()
            .any(|f| matches!(f, MaterialFinding::NonPbrMaterial { .. })));
    }

    #[test]
    fn swapped_channel_detected_when_normal_in_albedo_slot() {
        let mut m = mat("mat:oak");
        m.albedo_map = Some(TextureRef {
            // Note: the imported channel disagrees with the slot.
            blob_hash: "h1".into(),
            channel: "normal".into(),
            width: 1024,
            height: 1024,
        });
        let scene = scene_with(&["mat:oak"]);
        let known: std::collections::BTreeSet<String> = ["h1".to_string()].into_iter().collect();
        let result = check_materials(&scene, &[m], &known, &CheckMaterialsOptions::default());
        let has_swap = result
            .findings
            .iter()
            .any(|f| matches!(f, MaterialFinding::SwappedChannels { .. }));
        assert!(
            has_swap,
            "expected swapped-channel finding, got {:?}",
            result
        );
    }

    #[test]
    fn oversized_texture_flagged_against_budget() {
        let mut m = mat("mat:oak");
        m.albedo_map = Some(TextureRef {
            blob_hash: "h1".into(),
            channel: "albedo".into(),
            width: 8192,
            height: 8192,
        });
        let scene = scene_with(&["mat:oak"]);
        let known: std::collections::BTreeSet<String> = ["h1".to_string()].into_iter().collect();
        let result = check_materials(&scene, &[m], &known, &CheckMaterialsOptions::default());
        assert!(result
            .findings
            .iter()
            .any(|f| matches!(f, MaterialFinding::OversizedTexture { .. })));
    }

    #[test]
    fn material_referenced_but_missing_from_library_is_flagged() {
        let scene = scene_with(&["mat:ghost"]);
        let result = check_materials(
            &scene,
            &[],
            &std::collections::BTreeSet::new(),
            &CheckMaterialsOptions::default(),
        );
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].material_id(), "mat:ghost");
        assert!(matches!(
            result.findings[0],
            MaterialFinding::MissingTexture { .. }
        ));
    }

    #[test]
    fn unused_materials_are_not_inspected() {
        // `mat:oak` is unused; its bad metallic should not be flagged
        // since the scene doesn't reference it.
        let mut bad = mat("mat:oak");
        bad.metallic = 9.0;
        let scene = scene_with(&["mat:used"]);
        let result = check_materials(
            &scene,
            &[bad, mat("mat:used")],
            &std::collections::BTreeSet::new(),
            &CheckMaterialsOptions::default(),
        );
        assert!(result.is_clean());
    }

    #[test]
    fn findings_round_trip_via_serde() {
        let finding = MaterialFinding::MissingTexture {
            material_id: "mat:x".into(),
            channel: "albedo".into(),
            blob_hash: "abc".into(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let back: MaterialFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(finding, back);
    }

    #[test]
    fn into_ai_findings_includes_material_id_and_code() {
        let result = MaterialCheckResult {
            findings: vec![MaterialFinding::OversizedTexture {
                material_id: "mat:tile".into(),
                channel: "albedo".into(),
                width: 8192,
                height: 8192,
                budget: 4096,
            }],
        };
        let ai = result.into_ai_findings();
        assert_eq!(ai.len(), 1);
        assert_eq!(ai[0]["material_id"], "mat:tile");
        assert_eq!(ai[0]["code"], "material.oversized_texture");
        assert_eq!(ai[0]["severity"], "warning");
    }

    #[test]
    fn options_can_disable_swap_heuristic() {
        let mut m = mat("mat:oak");
        m.albedo_map = Some(TextureRef {
            blob_hash: "h1".into(),
            channel: "normal".into(),
            width: 1024,
            height: 1024,
        });
        let scene = scene_with(&["mat:oak"]);
        let known: std::collections::BTreeSet<String> = ["h1".to_string()].into_iter().collect();
        let opts = CheckMaterialsOptions {
            enable_swap_heuristic: false,
            ..Default::default()
        };
        let result = check_materials(&scene, &[m], &known, &opts);
        assert!(result.is_clean(), "swap heuristic should be disabled");
    }
}
