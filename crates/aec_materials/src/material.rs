//! PBR material struct shared between the viewport (real-time preview),
//! Blender worker (final render), and the IPC layer.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureRef {
    /// BLAKE3 hash key into the asset blob store.
    pub blob_hash: String,
    /// "albedo" | "metallic_roughness" | "normal" | "ao" | "emissive".
    pub channel: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PbrMaterial {
    pub id: String,
    pub name: String,
    /// Linear-space RGB albedo, each component in `[0.0, 1.0]`.
    pub albedo: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub ior: f32,
    pub emissive: [f32; 3],
    pub ao: f32,
    pub transmission: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub albedo_map: Option<TextureRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_map: Option<TextureRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic_roughness_map: Option<TextureRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ao_map: Option<TextureRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_map: Option<TextureRef>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub style_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
}

impl PbrMaterial {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            albedo: [0.6, 0.6, 0.6],
            metallic: 0.0,
            roughness: 0.6,
            ior: 1.45,
            emissive: [0.0; 3],
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
        }
    }

    pub fn with_albedo(mut self, rgb: [f32; 3]) -> Self {
        self.albedo = rgb;
        self
    }

    pub fn with_style_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.style_tags = tags.into_iter().collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_serde() {
        let m = PbrMaterial::new("mat:oak", "Light Oak")
            .with_albedo([0.78, 0.66, 0.5])
            .with_style_tags(["scandinavian".into(), "warm".into()]);
        let json = serde_json::to_string(&m).unwrap();
        let back: PbrMaterial = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
