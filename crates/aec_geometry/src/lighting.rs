//! Lighting presets shared between the viewport (preview), the native
//! CPU/GPU path tracer (final render), and the AI assistants.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingPresetId {
    WarmEvening,
    Daylight,
    Studio,
    OvercastNoon,
    GoldenHour,
}

impl LightingPresetId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WarmEvening => "warm_evening",
            Self::Daylight => "daylight",
            Self::Studio => "studio",
            Self::OvercastNoon => "overcast_noon",
            Self::GoldenHour => "golden_hour",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::WarmEvening,
            Self::Daylight,
            Self::Studio,
            Self::OvercastNoon,
            Self::GoldenHour,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingPreset {
    pub id: LightingPresetId,
    pub name: String,
    /// Color temperature of the dominant key light in Kelvin.
    pub key_temperature_k: u32,
    /// Sun azimuth in degrees (0 = north, 90 = east).
    pub sun_azimuth_deg: f64,
    /// Sun elevation in degrees (0 = horizon, 90 = zenith).
    pub sun_elevation_deg: f64,
    pub sun_intensity: f64,
    pub sky_intensity: f64,
    pub ambient_strength: f64,
    /// Sample bias to send to Cycles (higher = more samples for noisy
    /// lighting like dusk / interior dim).
    pub samples_multiplier: f64,
}

impl LightingPreset {
    pub fn from_id(id: LightingPresetId) -> Self {
        match id {
            LightingPresetId::WarmEvening => Self {
                id,
                name: "Warm evening".into(),
                key_temperature_k: 2700,
                sun_azimuth_deg: 270.0,
                sun_elevation_deg: 5.0,
                sun_intensity: 1.5,
                sky_intensity: 0.2,
                ambient_strength: 0.05,
                samples_multiplier: 1.25,
            },
            LightingPresetId::Daylight => Self {
                id,
                name: "Daylight".into(),
                key_temperature_k: 5500,
                sun_azimuth_deg: 180.0,
                sun_elevation_deg: 55.0,
                sun_intensity: 6.0,
                sky_intensity: 1.0,
                ambient_strength: 0.15,
                samples_multiplier: 1.0,
            },
            LightingPresetId::Studio => Self {
                id,
                name: "Studio".into(),
                key_temperature_k: 5600,
                sun_azimuth_deg: 135.0,
                sun_elevation_deg: 35.0,
                sun_intensity: 4.0,
                sky_intensity: 0.5,
                ambient_strength: 0.1,
                samples_multiplier: 1.1,
            },
            LightingPresetId::OvercastNoon => Self {
                id,
                name: "Overcast noon".into(),
                key_temperature_k: 6500,
                sun_azimuth_deg: 180.0,
                sun_elevation_deg: 75.0,
                sun_intensity: 1.0,
                sky_intensity: 4.0,
                ambient_strength: 0.5,
                samples_multiplier: 1.15,
            },
            LightingPresetId::GoldenHour => Self {
                id,
                name: "Golden hour".into(),
                key_temperature_k: 3500,
                sun_azimuth_deg: 280.0,
                sun_elevation_deg: 15.0,
                sun_intensity: 5.0,
                sky_intensity: 0.6,
                ambient_strength: 0.1,
                samples_multiplier: 1.2,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_have_distinct_intensities() {
        let presets: Vec<LightingPreset> = LightingPresetId::all()
            .iter()
            .map(|id| LightingPreset::from_id(*id))
            .collect();
        let mut intensities: Vec<f64> = presets.iter().map(|p| p.sun_intensity).collect();
        intensities.sort_by(|a, b| a.partial_cmp(b).unwrap());
        intensities.dedup();
        assert_eq!(intensities.len(), LightingPresetId::all().len());
    }

    #[test]
    fn warm_evening_uses_warm_temperature() {
        let p = LightingPreset::from_id(LightingPresetId::WarmEvening);
        assert!(p.key_temperature_k < 3500);
    }
}
