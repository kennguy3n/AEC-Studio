//! Lighting presets for the render pipeline.
//!
//! Each preset bakes a complete `Vec<RenderLight>` plus world / sky
//! parameters and ambient strength. Presets are addressable by id and
//! roundtripped via serde so the project package can persist user
//! choices and custom presets.
//!
//! IES profiles are loaded from disk via [`IesProfile::load_ies`] —
//! the parser accepts IES LM-63 family files (1986/1991/1995/2002) and
//! exposes the candela distribution plus normalisation metadata that
//! the Blender worker translates into light data nodes.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::scene::RenderLight;

/// Built-in lighting preset families.
///
/// The variants line up with ARCHITECTURE.md §6.4 — they're the moods
/// the Design page exposes as quick picks. Custom user presets are
/// stored in [`LightingPresetStore`] under their own ids and are not
/// part of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingPresetKind {
    WarmEvening,
    Daylight,
    Studio,
    GoldenHour,
    BlueTwilight,
    Overcast,
}

impl LightingPresetKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::WarmEvening => "warm_evening",
            Self::Daylight => "daylight",
            Self::Studio => "studio",
            Self::GoldenHour => "golden_hour",
            Self::BlueTwilight => "blue_twilight",
            Self::Overcast => "overcast",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::WarmEvening => "Warm Evening",
            Self::Daylight => "Daylight",
            Self::Studio => "Studio",
            Self::GoldenHour => "Golden Hour",
            Self::BlueTwilight => "Blue Twilight",
            Self::Overcast => "Overcast",
        }
    }

    pub fn all() -> [Self; 6] {
        [
            Self::WarmEvening,
            Self::Daylight,
            Self::Studio,
            Self::GoldenHour,
            Self::BlueTwilight,
            Self::Overcast,
        ]
    }
}

/// Sky/world parameters baked into a preset. The Blender worker
/// translates these into a `World` shader graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkyParams {
    /// Overall world strength multiplier.
    pub strength: f32,
    /// Sky tint linear RGB.
    pub color: [f32; 3],
    /// Atmospheric haze [0, 1]. Higher values soften shadows.
    pub turbidity: f32,
}

impl Default for SkyParams {
    /// Neutral overcast sky — strength 1.0, mid-grey tint, light haze.
    /// Used as a fallback world setup when no lighting preset has been
    /// applied. Picked so the scene is not pitch-black with no lights.
    fn default() -> Self {
        Self {
            strength: 1.0,
            color: [0.5, 0.5, 0.5],
            turbidity: 2.0,
        }
    }
}

/// A complete lighting setup. Fully self-describing: it carries every
/// light the worker should create plus the world tint and ambient
/// strength multiplier. The Blender worker reads `lights` directly
/// (the JSON form is `LightingPayload` below).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingPreset {
    pub id: String,
    pub display_name: String,
    pub kind: Option<LightingPresetKind>,
    /// Primary sun azimuth in degrees [0, 360).
    pub sun_azimuth_deg: f32,
    /// Sun elevation above horizon [-90, 90].
    pub sun_elevation_deg: f32,
    /// Sun intensity (W/m² roughly — Blender uses physical units).
    pub sun_intensity: f32,
    /// Sun color temperature in Kelvin (e.g. 6500 = noon, 3200 = tungsten).
    pub sun_color_temperature_k: f32,
    pub sky: SkyParams,
    pub ambient_strength: f32,
    /// Accent / fill lights. Sun is *not* duplicated here.
    pub accent_lights: Vec<RenderLight>,
}

impl LightingPreset {
    /// Materialise the full `Vec<RenderLight>` the renderer should
    /// upload, with the sun as the first entry followed by every
    /// accent light. Returning a fresh vec means callers can edit it
    /// without affecting the preset itself.
    pub fn build_lights(&self) -> Vec<RenderLight> {
        let mut out = Vec::with_capacity(self.accent_lights.len() + 1);
        out.push(RenderLight::SunSky {
            azimuth_deg: self.sun_azimuth_deg,
            elevation_deg: self.sun_elevation_deg,
            intensity: self.sun_intensity,
            color_temperature_k: self.sun_color_temperature_k,
        });
        out.extend(self.accent_lights.iter().cloned());
        out
    }

    /// Light intensity sanity check. Returns `Err` if the preset would
    /// produce a black render (no positive-energy lights).
    pub fn validate(&self) -> Result<(), LightingValidationError> {
        if !self.sun_intensity.is_finite() || self.sun_intensity < 0.0 {
            return Err(LightingValidationError::NegativeIntensity);
        }
        if !(1000.0..=12000.0).contains(&self.sun_color_temperature_k) {
            return Err(LightingValidationError::ColorTemperatureOutOfRange(
                self.sun_color_temperature_k,
            ));
        }
        // Sky and ambient illumination both contribute to the render —
        // a scene without a sun disc but with a strong sky still produces
        // a non-black image. Count them so purely-ambient outdoor presets
        // (e.g. overcast skies, interior light wells) validate cleanly.
        let accent_energy: f32 = self
            .accent_lights
            .iter()
            .map(|l| match l {
                RenderLight::SunSky { intensity, .. }
                | RenderLight::Area { intensity, .. }
                | RenderLight::Point { intensity, .. } => *intensity,
            })
            .sum::<f32>();
        let total_energy =
            self.sun_intensity + accent_energy + self.sky.strength + self.ambient_strength;
        if total_energy <= 0.0 {
            return Err(LightingValidationError::ZeroEnergy);
        }
        Ok(())
    }

    /// Construct one of the bundled presets by kind.
    pub fn from_kind(kind: LightingPresetKind) -> Self {
        match kind {
            LightingPresetKind::WarmEvening => Self::warm_evening(),
            LightingPresetKind::Daylight => Self::daylight(),
            LightingPresetKind::Studio => Self::studio(),
            LightingPresetKind::GoldenHour => Self::golden_hour(),
            LightingPresetKind::BlueTwilight => Self::blue_twilight(),
            LightingPresetKind::Overcast => Self::overcast(),
        }
    }

    pub fn warm_evening() -> Self {
        Self {
            id: "warm_evening".into(),
            display_name: "Warm Evening".into(),
            kind: Some(LightingPresetKind::WarmEvening),
            sun_azimuth_deg: 280.0,
            sun_elevation_deg: 8.0,
            sun_intensity: 1.5,
            sun_color_temperature_k: 3200.0,
            sky: SkyParams {
                strength: 0.2,
                color: [0.95, 0.55, 0.30],
                turbidity: 3.5,
            },
            ambient_strength: 0.25,
            accent_lights: vec![RenderLight::Area {
                position_mm: [0.0, 2500.0, 3000.0],
                width_mm: 2000.0,
                height_mm: 1500.0,
                intensity: 60.0,
                color_temperature_k: 2900.0,
            }],
        }
    }

    pub fn daylight() -> Self {
        Self {
            id: "daylight".into(),
            display_name: "Daylight".into(),
            kind: Some(LightingPresetKind::Daylight),
            sun_azimuth_deg: 150.0,
            sun_elevation_deg: 55.0,
            sun_intensity: 5.0,
            sun_color_temperature_k: 6500.0,
            sky: SkyParams {
                strength: 1.0,
                color: [0.65, 0.78, 0.95],
                turbidity: 2.0,
            },
            ambient_strength: 0.5,
            accent_lights: Vec::new(),
        }
    }

    pub fn studio() -> Self {
        Self {
            id: "studio".into(),
            display_name: "Studio".into(),
            kind: Some(LightingPresetKind::Studio),
            // Studio is rim/key/fill — sun is intentionally low.
            sun_azimuth_deg: 0.0,
            sun_elevation_deg: 0.0,
            sun_intensity: 0.1,
            sun_color_temperature_k: 5500.0,
            sky: SkyParams {
                strength: 0.0,
                color: [0.2, 0.2, 0.2],
                turbidity: 1.0,
            },
            ambient_strength: 0.1,
            accent_lights: vec![
                // Key light — large soft area at 45° camera-left.
                RenderLight::Area {
                    position_mm: [-2500.0, 1500.0, 2500.0],
                    width_mm: 2500.0,
                    height_mm: 2500.0,
                    intensity: 800.0,
                    color_temperature_k: 5500.0,
                },
                // Fill light — half-power, opposite side.
                RenderLight::Area {
                    position_mm: [2500.0, 1500.0, 2500.0],
                    width_mm: 2500.0,
                    height_mm: 2500.0,
                    intensity: 400.0,
                    color_temperature_k: 5500.0,
                },
                // Rim light — slightly cooler, behind subject.
                RenderLight::Point {
                    position_mm: [0.0, 3500.0, -1500.0],
                    intensity: 200.0,
                    color_temperature_k: 6200.0,
                },
            ],
        }
    }

    pub fn golden_hour() -> Self {
        Self {
            id: "golden_hour".into(),
            display_name: "Golden Hour".into(),
            kind: Some(LightingPresetKind::GoldenHour),
            sun_azimuth_deg: 250.0,
            sun_elevation_deg: 12.0,
            sun_intensity: 4.0,
            sun_color_temperature_k: 3800.0,
            sky: SkyParams {
                strength: 0.7,
                color: [0.95, 0.65, 0.4],
                turbidity: 5.0,
            },
            ambient_strength: 0.4,
            accent_lights: Vec::new(),
        }
    }

    pub fn blue_twilight() -> Self {
        Self {
            id: "blue_twilight".into(),
            display_name: "Blue Twilight".into(),
            kind: Some(LightingPresetKind::BlueTwilight),
            sun_azimuth_deg: 80.0,
            sun_elevation_deg: -5.0,
            sun_intensity: 0.5,
            sun_color_temperature_k: 9000.0,
            sky: SkyParams {
                strength: 0.4,
                color: [0.2, 0.35, 0.7],
                turbidity: 1.5,
            },
            ambient_strength: 0.3,
            accent_lights: vec![RenderLight::Point {
                position_mm: [1500.0, 1800.0, 1500.0],
                intensity: 60.0,
                color_temperature_k: 2700.0,
            }],
        }
    }

    pub fn overcast() -> Self {
        Self {
            id: "overcast".into(),
            display_name: "Overcast".into(),
            kind: Some(LightingPresetKind::Overcast),
            sun_azimuth_deg: 150.0,
            sun_elevation_deg: 35.0,
            sun_intensity: 1.2,
            sun_color_temperature_k: 6700.0,
            sky: SkyParams {
                strength: 1.4,
                color: [0.78, 0.80, 0.85],
                turbidity: 8.0,
            },
            ambient_strength: 0.65,
            accent_lights: Vec::new(),
        }
    }

    pub fn defaults() -> Vec<Self> {
        LightingPresetKind::all()
            .iter()
            .map(|k| Self::from_kind(*k))
            .collect()
    }

    /// JSON payload for the Blender worker. Matches the shape the
    /// existing `workers/blender/lighting.py::apply_lighting` accepts
    /// (top-level `name`, `lights`, `world`).
    pub fn worker_payload(&self) -> LightingPayload {
        let lights = self
            .build_lights()
            .iter()
            .enumerate()
            .map(|(idx, l)| WorkerLight::from_render_light(idx, l))
            .collect();
        LightingPayload {
            name: self.id.clone(),
            lights,
            world: WorkerWorld {
                strength: self.sky.strength,
                color: self.sky.color,
                turbidity: self.sky.turbidity,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightingPayload {
    pub name: String,
    pub lights: Vec<WorkerLight>,
    pub world: WorkerWorld,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerWorld {
    pub strength: f32,
    pub color: [f32; 3],
    pub turbidity: f32,
}

/// The flat light spec used in the JSON-over-stdio worker protocol.
/// Mirrors the `lighting.py` accepted shape (`type`, `energy`, `color`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerLight {
    pub name: String,
    #[serde(rename = "type")]
    pub light_type: String,
    pub energy: f32,
    pub color: [f32; 3],
}

impl WorkerLight {
    pub fn from_render_light(idx: usize, light: &RenderLight) -> Self {
        match light {
            RenderLight::SunSky {
                intensity,
                color_temperature_k,
                ..
            } => Self {
                name: format!("Sun_{idx}"),
                light_type: "SUN".into(),
                energy: *intensity,
                color: kelvin_to_rgb(*color_temperature_k),
            },
            RenderLight::Area {
                intensity,
                color_temperature_k,
                ..
            } => Self {
                name: format!("Area_{idx}"),
                light_type: "AREA".into(),
                energy: *intensity,
                color: kelvin_to_rgb(*color_temperature_k),
            },
            RenderLight::Point {
                intensity,
                color_temperature_k,
                ..
            } => Self {
                name: format!("Point_{idx}"),
                light_type: "POINT".into(),
                energy: *intensity,
                color: kelvin_to_rgb(*color_temperature_k),
            },
        }
    }
}

/// Convert a Kelvin color temperature to linear RGB using the
/// Tanner Helland approximation
/// (<https://tannerhelland.com/2012/09/18/convert-temperature-rgb-algorithm-code.html>).
/// Output is clamped to [0, 1] linear floats so it can be passed
/// straight to Blender's light `color` attribute.
pub fn kelvin_to_rgb(kelvin_k: f32) -> [f32; 3] {
    // Tanner Helland's piecewise polynomial. The constants are baked
    // and have been used in countless renderers for decades; the
    // approximation matches a 1000K..40000K range and is more than
    // adequate for our 1000..12000K UI bounds. We compute in sRGB
    // 8-bit and divide by 255 at the end.
    let t = (kelvin_k / 100.0).clamp(10.0, 400.0);

    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_73 * (t - 60.0).powf(-0.133_204_8)
    };
    let g = if t <= 66.0 {
        99.470_8 * t.ln() - 161.119_57
    } else {
        288.122_2 * (t - 60.0).powf(-0.075_514_85)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_73 * (t - 10.0).ln() - 305.044_8
    };
    [
        (r / 255.0).clamp(0.0, 1.0),
        (g / 255.0).clamp(0.0, 1.0),
        (b / 255.0).clamp(0.0, 1.0),
    ]
}

/// IES (LM-63) photometric profile. Stores the parsed candela
/// distribution table plus the normalisation metadata fixture-makers
/// need: vertical/horizontal angle counts, lumens-per-lamp, and the
/// candela multiplier. Useful for picture-rated fixtures (cans,
/// architectural luminaires) whose distribution is part of the spec.
#[derive(Debug, Clone, PartialEq)]
pub struct IesProfile {
    pub source: String,
    pub lamp_count: u32,
    pub lumens_per_lamp: f32,
    pub candela_multiplier: f32,
    pub vertical_angles: Vec<f32>,
    pub horizontal_angles: Vec<f32>,
    /// Candela values laid out as `horizontal × vertical`. Indexed
    /// `[h_idx * vertical_angles.len() + v_idx]`.
    pub candela: Vec<f32>,
    pub photometric_type: IesPhotometricType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IesPhotometricType {
    /// Type C — most architectural fixtures.
    C = 1,
    /// Type B — floodlights.
    B = 2,
    /// Type A — automotive headlamps.
    A = 3,
}

impl IesPhotometricType {
    fn parse(token: &str) -> Result<Self, IesParseError> {
        match token.trim() {
            "1" => Ok(Self::C),
            "2" => Ok(Self::B),
            "3" => Ok(Self::A),
            other => Err(IesParseError::UnknownPhotometricType(other.into())),
        }
    }
}

#[derive(Debug, Error)]
pub enum IesParseError {
    #[error("io error reading IES file: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing TILT= line (mandatory in LM-63)")]
    MissingTilt,
    #[error("malformed numeric field at position {0}: {1}")]
    MalformedNumber(usize, String),
    #[error("unknown photometric type token `{0}`")]
    UnknownPhotometricType(String),
    #[error("candela table truncated: expected {expected} values, got {got}")]
    TruncatedCandelaTable { expected: usize, got: usize },
}

impl IesProfile {
    /// Load an LM-63 IES file from disk.
    pub fn load_ies(path: impl AsRef<Path>) -> Result<Self, IesParseError> {
        let text = std::fs::read_to_string(path)?;
        Self::parse_ies(&text)
    }

    /// Parse an LM-63 IES file from a string. Tolerant of LM-63-1986,
    /// 1991, 1995 and 2002 header variants — every modern file starts
    /// with a `IESNA:LM-63-…` line and ends the header with `TILT=`.
    pub fn parse_ies(text: &str) -> Result<Self, IesParseError> {
        // Header: keywords run until we hit a `TILT=` line. We capture
        // the file source from the first comment line or default to
        // `(unknown)`.
        let mut lines = text.lines();
        let mut source = String::new();
        let mut tilt_seen = false;
        for line in lines.by_ref() {
            let line = line.trim();
            if line.starts_with('[') && source.is_empty() {
                // [TEST=] [MANUFAC=] keywords — first non-empty becomes our source.
                source = line.to_string();
            }
            if line.starts_with("TILT=") {
                tilt_seen = true;
                break;
            }
        }
        if !tilt_seen {
            return Err(IesParseError::MissingTilt);
        }

        // After TILT= comes a sequence of whitespace-separated numeric
        // tokens. The LM-63 spec is fixed:
        //   line 10 (after TILT): num_lamps lumens_per_lamp candela_mult num_v num_h photo_type units width length height
        //   line 11: ballast_factor ballast-lamp_factor input_watts
        //   then: vertical_angles (num_v values)
        //   then: horizontal_angles (num_h values)
        //   then: candela values (num_v * num_h values, row-major H then V)
        let mut tokens: Vec<&str> = Vec::new();
        for rest in lines {
            tokens.extend(rest.split_ascii_whitespace());
        }

        let mut cursor = 0usize;
        let take_f32 = |tokens: &[&str], idx: &mut usize| -> Result<f32, IesParseError> {
            let tok = tokens
                .get(*idx)
                .copied()
                .ok_or(IesParseError::TruncatedCandelaTable {
                    expected: *idx + 1,
                    got: tokens.len(),
                })?;
            *idx += 1;
            tok.parse::<f32>()
                .map_err(|_| IesParseError::MalformedNumber(*idx - 1, tok.to_string()))
        };
        let take_u32 = |tokens: &[&str], idx: &mut usize| -> Result<u32, IesParseError> {
            let v = take_f32(tokens, idx)?;
            Ok(v as u32)
        };

        let lamp_count = take_u32(&tokens, &mut cursor)?;
        let lumens_per_lamp = take_f32(&tokens, &mut cursor)?;
        let candela_multiplier = take_f32(&tokens, &mut cursor)?;
        let num_v = take_u32(&tokens, &mut cursor)? as usize;
        let num_h = take_u32(&tokens, &mut cursor)? as usize;
        let photo_type =
            tokens
                .get(cursor)
                .copied()
                .ok_or(IesParseError::TruncatedCandelaTable {
                    expected: cursor + 1,
                    got: tokens.len(),
                })?;
        let photometric_type = IesPhotometricType::parse(photo_type)?;
        cursor += 1;
        // skip units, width, length, height
        for _ in 0..4 {
            take_f32(&tokens, &mut cursor)?;
        }
        // skip ballast factor + ballast-lamp factor + input watts
        for _ in 0..3 {
            take_f32(&tokens, &mut cursor)?;
        }

        let mut vertical_angles = Vec::with_capacity(num_v);
        for _ in 0..num_v {
            vertical_angles.push(take_f32(&tokens, &mut cursor)?);
        }
        let mut horizontal_angles = Vec::with_capacity(num_h);
        for _ in 0..num_h {
            horizontal_angles.push(take_f32(&tokens, &mut cursor)?);
        }
        let candela_count = num_v.saturating_mul(num_h);
        let mut candela = Vec::with_capacity(candela_count);
        for _ in 0..candela_count {
            candela.push(take_f32(&tokens, &mut cursor)?);
        }
        if candela.len() != candela_count {
            return Err(IesParseError::TruncatedCandelaTable {
                expected: candela_count,
                got: candela.len(),
            });
        }
        Ok(Self {
            source,
            lamp_count,
            lumens_per_lamp,
            candela_multiplier,
            vertical_angles,
            horizontal_angles,
            candela,
            photometric_type,
        })
    }

    /// Peak candela value across the entire distribution.
    pub fn peak_candela(&self) -> f32 {
        self.candela.iter().copied().fold(0.0_f32, f32::max) * self.candela_multiplier
    }

    /// Bilinearly-interpolated candela value at the supplied vertical /
    /// horizontal angles, in degrees. Vertical is measured from the
    /// luminaire's downward axis; horizontal is measured around it.
    ///
    /// Out-of-range angles clamp to the nearest sampled angle (so a
    /// type-C distribution that only spans 0..=90° returns its boundary
    /// value above 90°, rather than zero).
    pub fn candela_at(&self, vertical_deg: f32, horizontal_deg: f32) -> f32 {
        if self.vertical_angles.is_empty() || self.horizontal_angles.is_empty() {
            return 0.0;
        }
        let (v0_idx, v_t) = bracket_angle(&self.vertical_angles, vertical_deg);
        let (h0_idx, h_t) = bracket_angle(&self.horizontal_angles, horizontal_deg);
        let v_len = self.vertical_angles.len();
        let h_len = self.horizontal_angles.len();
        let v1_idx = (v0_idx + 1).min(v_len - 1);
        let h1_idx = (h0_idx + 1).min(h_len - 1);
        let sample = |h: usize, v: usize| -> f32 { self.candela[h * v_len + v] };
        let c00 = sample(h0_idx, v0_idx);
        let c01 = sample(h0_idx, v1_idx);
        let c10 = sample(h1_idx, v0_idx);
        let c11 = sample(h1_idx, v1_idx);
        let c0 = c00 + (c01 - c00) * v_t;
        let c1 = c10 + (c11 - c10) * v_t;
        let raw = c0 + (c1 - c0) * h_t;
        raw * self.candela_multiplier
    }

    /// Synthetic profile used by unit tests — emits `peak_cd` candela
    /// uniformly across the full sphere.
    pub fn test_isotropic(peak_cd: f32) -> Self {
        let vertical_angles: Vec<f32> = (0..=18).map(|i| i as f32 * 10.0).collect();
        let horizontal_angles: Vec<f32> = vec![0.0];
        let candela = vec![peak_cd; vertical_angles.len() * horizontal_angles.len()];
        Self {
            source: "synthetic isotropic".into(),
            lamp_count: 1,
            lumens_per_lamp: peak_cd * 4.0 * std::f32::consts::PI,
            candela_multiplier: 1.0,
            vertical_angles,
            horizontal_angles,
            candela,
            photometric_type: IesPhotometricType::C,
        }
    }
}

/// Find `(lower_index, t)` such that `angles[lower_index] <= a <= angles[lower_index+1]`
/// and `t in [0, 1]` is the interpolation parameter. Clamps to the
/// boundary when `a` is outside the sampled range.
fn bracket_angle(angles: &[f32], a: f32) -> (usize, f32) {
    if angles.len() < 2 {
        return (0, 0.0);
    }
    if a <= angles[0] {
        return (0, 0.0);
    }
    if a >= angles[angles.len() - 1] {
        return (angles.len() - 2, 1.0);
    }
    // Binary search for the first index whose angle exceeds `a`.
    let upper = angles
        .partition_point(|&v| v <= a)
        .max(1)
        .min(angles.len() - 1);
    let lower = upper - 1;
    let span = (angles[upper] - angles[lower]).max(1e-9);
    let t = (a - angles[lower]) / span;
    (lower, t)
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum LightingValidationError {
    #[error("sun intensity must be non-negative and finite")]
    NegativeIntensity,
    #[error("color temperature {0} K is outside the supported 1000..=12000 K range")]
    ColorTemperatureOutOfRange(f32),
    #[error("preset has zero positive-energy lights; render would be black")]
    ZeroEnergy,
}

/// Registry of lighting presets — bundled defaults plus user-saved customs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingPresetStore {
    pub custom: BTreeMap<String, LightingPreset>,
    pub selected: String,
}

impl Default for LightingPresetStore {
    fn default() -> Self {
        Self {
            custom: BTreeMap::new(),
            selected: LightingPresetKind::Daylight.id().into(),
        }
    }
}

impl LightingPresetStore {
    pub fn all(&self) -> Vec<LightingPreset> {
        let mut out = LightingPreset::defaults();
        out.extend(self.custom.values().cloned());
        out
    }

    pub fn get(&self, id: &str) -> Option<LightingPreset> {
        LightingPreset::defaults()
            .into_iter()
            .find(|p| p.id == id)
            .or_else(|| self.custom.get(id).cloned())
    }

    pub fn current(&self) -> LightingPreset {
        self.get(&self.selected)
            .unwrap_or_else(LightingPreset::daylight)
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.get(id).is_some() {
            self.selected = id.to_string();
            true
        } else {
            false
        }
    }

    pub fn insert_custom(&mut self, preset: LightingPreset) -> Result<(), LightingValidationError> {
        preset.validate()?;
        self.custom.insert(preset.id.clone(), preset);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_preset_validates() {
        for p in LightingPreset::defaults() {
            p.validate().unwrap_or_else(|e| {
                panic!("default preset `{}` failed validation: {e}", p.id);
            });
        }
    }

    #[test]
    fn build_lights_prepends_sun() {
        let p = LightingPreset::warm_evening();
        let lights = p.build_lights();
        assert!(matches!(lights[0], RenderLight::SunSky { .. }));
        assert_eq!(lights.len(), 1 + p.accent_lights.len());
    }

    #[test]
    fn studio_has_key_fill_rim() {
        let s = LightingPreset::studio();
        assert_eq!(s.accent_lights.len(), 3);
    }

    #[test]
    fn validate_rejects_zero_energy_preset() {
        let mut p = LightingPreset::overcast();
        p.sun_intensity = 0.0;
        p.accent_lights.clear();
        // Ambient + sky also count toward the energy budget — zero them
        // out so this case truly has no light contribution.
        p.sky.strength = 0.0;
        p.ambient_strength = 0.0;
        assert_eq!(p.validate(), Err(LightingValidationError::ZeroEnergy));
    }

    #[test]
    fn validate_accepts_sky_only_preset() {
        // A purely sky-lit scene (no sun disc, no accent lights, no
        // ambient) — e.g. an open-air overcast scene — should validate
        // because the sky shader still produces light.
        let mut p = LightingPreset::overcast();
        p.sun_intensity = 0.0;
        p.accent_lights.clear();
        p.ambient_strength = 0.0;
        p.sky.strength = 1.5;
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_accepts_ambient_only_preset() {
        // Pure ambient (interior scenes lit only by their environment).
        let mut p = LightingPreset::overcast();
        p.sun_intensity = 0.0;
        p.accent_lights.clear();
        p.sky.strength = 0.0;
        p.ambient_strength = 0.5;
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_temperature() {
        let mut p = LightingPreset::daylight();
        p.sun_color_temperature_k = 30_000.0;
        assert!(matches!(
            p.validate(),
            Err(LightingValidationError::ColorTemperatureOutOfRange(_))
        ));
    }

    #[test]
    fn kelvin_to_rgb_is_warm_at_low_temperatures() {
        // Tungsten ~ 3200K should be warm (R > B).
        let rgb = kelvin_to_rgb(3200.0);
        assert!(rgb[0] > rgb[2]);
        // Cool ~ 9000K should have B > R.
        let cool = kelvin_to_rgb(9000.0);
        assert!(cool[2] > cool[0]);
    }

    #[test]
    fn worker_payload_includes_sun_first() {
        let payload = LightingPreset::studio().worker_payload();
        assert!(payload.lights[0].light_type == "SUN");
        // Studio has 3 accents + 1 sun.
        assert_eq!(payload.lights.len(), 4);
    }

    #[test]
    fn store_roundtrips_via_serde() {
        let mut store = LightingPresetStore::default();
        let mut custom = LightingPreset::warm_evening();
        custom.id = "warm_evening_high".into();
        custom.sun_intensity = 2.5;
        store.insert_custom(custom.clone()).unwrap();
        assert!(store.select("warm_evening_high"));

        let bytes = serde_json::to_vec(&store).unwrap();
        let loaded: LightingPresetStore = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(loaded, store);
        assert_eq!(loaded.current().id, "warm_evening_high");
    }

    #[test]
    fn store_select_rejects_unknown_ids() {
        let mut store = LightingPresetStore::default();
        let orig = store.selected.clone();
        assert!(!store.select("nope"));
        assert_eq!(store.selected, orig);
    }

    /// Fixture: a minimal, hand-verified LM-63-2002 IES file. Five
    /// vertical angles × one horizontal angle = 5 candela samples.
    /// Candela multiplier = 1.0, so peak candela == max raw value (10.0).
    const SAMPLE_IES: &str = r#"IESNA:LM-63-2002
[TEST=Cognition AEC Studio fixture]
[MANUFAC=ACME]
TILT=NONE
1 1000.0 1.0 5 1 1 2 0.0 0.0 0.0
1.0 1.0 100.0
0.0 22.5 45.0 67.5 90.0
0.0
10.0 8.0 6.0 4.0 2.0
"#;

    #[test]
    fn ies_parser_extracts_distribution() {
        let p = IesProfile::parse_ies(SAMPLE_IES).unwrap();
        assert_eq!(p.lamp_count, 1);
        assert_eq!(p.lumens_per_lamp, 1000.0);
        assert_eq!(p.candela_multiplier, 1.0);
        assert_eq!(p.vertical_angles.len(), 5);
        assert_eq!(p.horizontal_angles.len(), 1);
        assert_eq!(p.candela, vec![10.0, 8.0, 6.0, 4.0, 2.0]);
        assert_eq!(p.peak_candela(), 10.0);
        assert_eq!(p.photometric_type, IesPhotometricType::C);
    }

    #[test]
    fn ies_parser_rejects_missing_tilt() {
        let bad = "IESNA:LM-63-2002\n1 1000.0 1.0 1 1 1 2 0 0 0\n1 1 1\n0\n0\n0\n";
        assert!(matches!(
            IesProfile::parse_ies(bad).unwrap_err(),
            IesParseError::MissingTilt
        ));
    }

    #[test]
    fn ies_parser_rejects_truncated_table() {
        // num_v=5, num_h=1, so we need 5 candela values, but only 3 are
        // provided. Removing two from the end of SAMPLE_IES.
        let truncated = r#"IESNA:LM-63-2002
TILT=NONE
1 1000.0 1.0 5 1 1 2 0.0 0.0 0.0
1.0 1.0 100.0
0.0 22.5 45.0 67.5 90.0
0.0
10.0 8.0 6.0
"#;
        let err = IesProfile::parse_ies(truncated).unwrap_err();
        assert!(matches!(err, IesParseError::TruncatedCandelaTable { .. }));
    }
}
