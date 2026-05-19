//! Render presets. Mirror the table in `ARCHITECTURE.md`:
//! Quick=32, Standard=128, High=256, Studio=1024.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderQuality {
    Eevee,
    Quick,
    Standard,
    High,
    Studio,
    Walkthrough,
    Panorama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPresetConfig {
    pub quality: RenderQuality,
    /// Sample count for Cycles; for EEVEE this is the temporal sample count.
    pub samples: u32,
    pub denoise: bool,
    pub tile_size_px: u32,
    pub resolution_x: u32,
    pub resolution_y: u32,
    pub use_motion_blur: bool,
    pub use_volumetric_atmosphere: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPreset {
    pub id: String,
    pub display_name: String,
    pub config: RenderPresetConfig,
}

impl RenderPreset {
    pub fn eevee_preview() -> Self {
        Self {
            id: "eevee_preview".into(),
            display_name: "EEVEE preview".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Eevee,
                samples: 64,
                denoise: false,
                tile_size_px: 256,
                resolution_x: 1280,
                resolution_y: 720,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn quick() -> Self {
        Self {
            id: "cycles_quick".into(),
            display_name: "Quick".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Quick,
                samples: 32,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1280,
                resolution_y: 720,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn standard() -> Self {
        Self {
            id: "cycles_standard".into(),
            display_name: "Standard".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Standard,
                samples: 128,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn high() -> Self {
        Self {
            id: "cycles_high".into(),
            display_name: "High".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::High,
                samples: 256,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: false,
                use_volumetric_atmosphere: true,
            },
        }
    }

    pub fn studio() -> Self {
        Self {
            id: "cycles_studio".into(),
            display_name: "Studio".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Studio,
                samples: 1024,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 3840,
                resolution_y: 2160,
                use_motion_blur: true,
                use_volumetric_atmosphere: true,
            },
        }
    }

    pub fn walkthrough() -> Self {
        Self {
            id: "cycles_walkthrough".into(),
            display_name: "Walkthrough".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Walkthrough,
                samples: 96,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: true,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn panorama() -> Self {
        Self {
            id: "cycles_panorama".into(),
            display_name: "Panorama".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Panorama,
                samples: 512,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 4096,
                resolution_y: 2048,
                use_motion_blur: false,
                use_volumetric_atmosphere: true,
            },
        }
    }

    /// All bundled presets.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self::eevee_preview(),
            Self::quick(),
            Self::standard(),
            Self::high(),
            Self::studio(),
            Self::walkthrough(),
            Self::panorama(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_increasing_samples() {
        assert!(RenderPreset::quick().config.samples < RenderPreset::standard().config.samples);
        assert!(RenderPreset::standard().config.samples < RenderPreset::high().config.samples);
        assert!(RenderPreset::high().config.samples < RenderPreset::studio().config.samples);
    }

    #[test]
    fn defaults_are_unique_ids() {
        let presets = RenderPreset::defaults();
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
    }
}
