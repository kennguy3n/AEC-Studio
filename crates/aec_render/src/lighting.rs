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
//! the native renderer maps onto [`crate::light_sampling::NativeLight::Ies`]
//! at scene-build time.

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

/// Sky/world parameters baked into a preset. The native renderer
/// translates these into the Preetham sky kernel (see
/// [`crate::light_sampling::environment_radiance`] and the
/// `aec_viewport::sky` shader).
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
/// light the renderer should create plus the world tint and ambient
/// strength multiplier. Callers feed `lights` directly to the native
/// path tracer (see [`crate::light_sampling::NativeLight::from_render_light`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingPreset {
    pub id: String,
    pub display_name: String,
    pub kind: Option<LightingPresetKind>,
    /// Primary sun azimuth in degrees [0, 360).
    pub sun_azimuth_deg: f32,
    /// Sun elevation above horizon [-90, 90].
    pub sun_elevation_deg: f32,
    /// Sun intensity (W/m² roughly — same physical-units convention
    /// the native path tracer uses for direct sun radiance).
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
}

/// Convert a Kelvin color temperature to linear RGB using the
/// Tanner Helland approximation
/// (<https://tannerhelland.com/2012/09/18/convert-temperature-rgb-algorithm-code.html>).
/// Output is clamped to [0, 1] linear floats so it can be passed
/// directly into [`crate::light_sampling::NativeLight`] colour fields
/// and into the preview rasteriser's per-light colour uniform.
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

    /// Total luminous flux declared by the IES file, in lumens.
    /// `lamp_count × lumens_per_lamp` per LM-63 spec. Returns `None`
    /// when the file did not declare a positive lumen value (some
    /// fixtures use `-1` to indicate "absolute photometry").
    pub fn declared_lumens(&self) -> Option<f32> {
        if self.lumens_per_lamp > 0.0 {
            Some(self.lumens_per_lamp * self.lamp_count.max(1) as f32)
        } else {
            None
        }
    }

    /// Numerically integrate the candela distribution over the full
    /// sphere using the trapezoidal rule on the sampled (vertical,
    /// horizontal) grid:
    ///
    /// ```text
    ///   Φ = ∫₀^{2π} ∫₀^π I(θ, φ) · sin(θ) · dθ dφ
    /// ```
    ///
    /// For rotationally-symmetric distributions (a single sampled
    /// horizontal angle), the inner integral collapses to `2π · I(θ) ·
    /// sin(θ) · dθ`. For partial horizontal sweeps (0..=90°, 0..=180°)
    /// the result is scaled up by the implied symmetry factor (4, 2)
    /// — matching the LM-63 convention that a `[0, 90°]` horizontal
    /// range implies a luminaire with four-way symmetry.
    pub fn integrate_lumens(&self) -> f32 {
        let v = &self.vertical_angles;
        let h = &self.horizontal_angles;
        if v.len() < 2 || h.is_empty() {
            return 0.0;
        }
        let cd = |hi: usize, vi: usize| -> f64 { self.candela[hi * v.len() + vi] as f64 };

        let mut flux = 0.0_f64;

        if h.len() == 1 {
            // Rotationally symmetric — revolve around the vertical
            // axis. Φ = 2π · Σᵢ avg(I_i, I_{i+1}) · sin(θ_avg) · dθ.
            for i in 0..v.len() - 1 {
                let t0 = (v[i] as f64).to_radians();
                let t1 = (v[i + 1] as f64).to_radians();
                let dtheta = t1 - t0;
                // Mid-point sin(θ) is more accurate than trapezoidal
                // sin endpoints because sin(0)=0 vanishes at the pole.
                let sin_mid = ((t0 + t1) * 0.5).sin();
                let cd_avg = 0.5 * (cd(0, i) + cd(0, i + 1));
                flux += 2.0 * std::f64::consts::PI * cd_avg * sin_mid * dtheta;
            }
        } else {
            for i in 0..v.len() - 1 {
                let t0 = (v[i] as f64).to_radians();
                let t1 = (v[i + 1] as f64).to_radians();
                let dtheta = t1 - t0;
                let sin_mid = ((t0 + t1) * 0.5).sin();
                for j in 0..h.len() - 1 {
                    let p0 = (h[j] as f64).to_radians();
                    let p1 = (h[j + 1] as f64).to_radians();
                    let dphi = p1 - p0;
                    // 4-corner average over the (θ, φ) cell.
                    let cd_avg = 0.25 * (cd(j, i) + cd(j + 1, i) + cd(j, i + 1) + cd(j + 1, i + 1));
                    flux += cd_avg * sin_mid * dtheta * dphi;
                }
            }
            // Scale up by the implied LM-63 symmetry.
            let span = (h.last().copied().unwrap_or(0.0) - h[0]) as f64;
            if span > 0.0 {
                let factor = 360.0 / span;
                flux *= factor;
            }
        }

        (flux * self.candela_multiplier as f64) as f32
    }

    /// Bake the photometric distribution into a rectified
    /// (`horizontal_resolution` × `vertical_resolution`) f32 candela
    /// lookup texture. Layout is row-major with `vertical_resolution`
    /// rows and `horizontal_resolution` columns; row `y` corresponds
    /// to a vertical angle of `y / (height - 1) · 180°`, column `x`
    /// to a horizontal angle of `x / (width - 1) · 360°`. The
    /// returned buffer is `width × height` floats including the
    /// `candela_multiplier` so a path-tracer's GPU sampler can read
    /// it directly without further scaling.
    ///
    /// The bake uses the same bilinear `candela_at` interpolator that
    /// the CPU light sampler uses, so CPU and GPU samples agree to
    /// within float precision.
    pub fn to_lookup_texture(
        &self,
        horizontal_resolution: u32,
        vertical_resolution: u32,
    ) -> IesLookupTexture {
        let w = horizontal_resolution.max(1) as usize;
        let h = vertical_resolution.max(1) as usize;
        let mut data = vec![0.0_f32; w * h];
        for y in 0..h {
            let v_deg = if h > 1 {
                (y as f32 / (h - 1) as f32) * 180.0
            } else {
                0.0
            };
            for x in 0..w {
                let h_deg = if w > 1 {
                    (x as f32 / (w - 1) as f32) * 360.0
                } else {
                    0.0
                };
                data[y * w + x] = self.candela_at(v_deg, h_deg);
            }
        }
        IesLookupTexture {
            width: w as u32,
            height: h as u32,
            candela: data,
        }
    }

    /// Bilinearly-interpolated candela value at the supplied vertical /
    /// horizontal angles, in degrees. Vertical is measured from the
    /// luminaire's downward axis; horizontal is measured around it.
    ///
    /// **Horizontal-plane symmetry** is applied per the LM-63 implicit
    /// convention (encoded by the horizontal-angle span of the file):
    ///
    /// * a single horizontal sample (rotational symmetry): any input
    ///   returns that single sample.
    /// * span ≈ 90° (4-way / quadrant symmetry): inputs fold by
    ///   reflection about the 90° and 180° axes into `[0°, 90°]`.
    /// * span ≈ 180° (bilateral symmetry): inputs fold by reflection
    ///   about the 180° plane into `[0°, 180°]`.
    /// * span ≈ 360° (full sweep, no implicit symmetry): inputs wrap
    ///   via `rem_euclid(360°)`.
    /// * non-standard span (LM-63 doesn't define implicit symmetry for
    ///   such files): out-of-range inputs clamp to the nearest sampled
    ///   horizontal angle, same as vertical.
    ///
    /// Vertical inputs outside the sampled range clamp to the nearest
    /// boundary (e.g. a type-C distribution sampled only on `0..=90°`
    /// returns its 90° row for any vertical input > 90°).
    pub fn candela_at(&self, vertical_deg: f32, horizontal_deg: f32) -> f32 {
        if self.vertical_angles.is_empty() || self.horizontal_angles.is_empty() {
            return 0.0;
        }
        let folded_h = fold_horizontal_for_symmetry(&self.horizontal_angles, horizontal_deg);
        let (v0_idx, v_t) = bracket_angle(&self.vertical_angles, vertical_deg);
        let (h0_idx, h_t) = bracket_angle(&self.horizontal_angles, folded_h);
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

/// Rectified candela lookup texture, ready for upload to a GPU
/// sampler or use by the CPU path tracer. The candela values
/// include the source IES file's `candela_multiplier`.
#[derive(Debug, Clone, PartialEq)]
pub struct IesLookupTexture {
    /// Number of columns (horizontal-angle steps, spanning 0..=360°).
    pub width: u32,
    /// Number of rows (vertical-angle steps, spanning 0..=180°).
    pub height: u32,
    /// Row-major candela values: `candela[y * width + x]`.
    pub candela: Vec<f32>,
}

impl IesLookupTexture {
    /// Bilinearly sample the texture at the supplied vertical /
    /// horizontal angles in degrees.
    ///
    /// Both axes use the **endpoint-inclusive** parameterization that
    /// [`IesProfile::to_lookup_texture`] writes: column `x` corresponds
    /// to horizontal angle `x / (width - 1) · 360°` and row `y` to
    /// vertical angle `y / (height - 1) · 180°`. The sampler MUST mirror
    /// that mapping or it reads from a non-existent in-between column
    /// — e.g. on a 4-wide texture, an input of 120° (which the bake
    /// stored exactly in column 1) would otherwise interpolate between
    /// columns 1 and 2, returning a blend of cd(120°) and cd(240°)
    /// instead of cd(120°).
    ///
    /// Horizontal wraps via `rem_euclid` (so 360°, 720°, −10° all map
    /// back into `[0°, 360°)`); 360° therefore samples column 0. The
    /// bake guarantees periodicity (column 0 == column `width − 1`)
    /// regardless of the source IES file's horizontal span:
    /// [`IesProfile::to_lookup_texture`] folds source samples through
    /// the LM-63 implicit-symmetry convention
    /// (rotational / 4-way / bilateral / full-sweep) inside
    /// [`IesProfile::candela_at`] before writing each texel, so even
    /// quadrant-symmetric (0..=90°) and bilaterally-symmetric (0..=180°)
    /// profiles produce a baked texture with no wrap discontinuity at
    /// 360°. Vertical clamps to `[0°, 180°]`.
    pub fn sample(&self, vertical_deg: f32, horizontal_deg: f32) -> f32 {
        // `.max(1)` guarantees both axes are at least 1; degenerate
        // textures collapse onto column / row 0 rather than panicking.
        let w = self.width.max(1) as usize;
        let h = self.height.max(1) as usize;
        // If a caller constructed `IesLookupTexture` directly with an
        // empty `candela` buffer (the fields are `pub`), bail out
        // rather than indexing past the end. The bake itself always
        // allocates `w × h` floats so this only catches user error.
        if self.candela.len() < w * h {
            return 0.0;
        }
        let vy = (vertical_deg.clamp(0.0, 180.0) / 180.0) * (h as f32 - 1.0);
        // Endpoint-inclusive: map [0°, 360°) onto [0, w-1]. With w=1
        // there is only one column, so hx collapses to 0 regardless of
        // the input angle. The bake stores cd(360°) in column `w-1`
        // which equals cd(0°), so wrapping a 360° input back to column
        // 0 preserves continuity.
        let hx = if w > 1 {
            horizontal_deg.rem_euclid(360.0) / 360.0 * (w as f32 - 1.0)
        } else {
            0.0
        };
        let y0 = vy.floor() as usize;
        let y1 = (y0 + 1).min(h - 1);
        let ty = vy - y0 as f32;
        // Clamp (rather than modulo) so `x0`/`x1` stay inside the
        // endpoint-inclusive column range. The `rem_euclid` above
        // already collapses 360° back to column 0, so we never need
        // the wraparound branch.
        let x0 = (hx.floor() as usize).min(w.saturating_sub(1));
        let x1 = (x0 + 1).min(w - 1);
        let tx = hx - x0 as f32;
        let c00 = self.candela[y0 * w + x0];
        let c10 = self.candela[y0 * w + x1];
        let c01 = self.candela[y1 * w + x0];
        let c11 = self.candela[y1 * w + x1];
        let c0 = c00 + (c10 - c00) * tx;
        let c1 = c01 + (c11 - c01) * tx;
        c0 + (c1 - c0) * ty
    }
}

/// Fold an input horizontal angle into the IES file's sampled
/// horizontal range using the LM-63 implicit-symmetry convention.
///
/// LM-63 IES files encode horizontal-plane symmetry implicitly via the
/// horizontal-angle span:
///
/// * **single angle** (one horizontal entry): rotational symmetry —
///   any input maps to that single sample.
/// * **span ≈ 90°** (e.g. `[0, 45, 90]`): 4-way (quadrant) symmetry.
///   Reflect inputs about the 90° and 180° axes to fold into `[0°, 90°]`.
/// * **span ≈ 180°** (e.g. `[0, 90, 180]`): bilateral (left-right)
///   symmetry about the file's reference plane. Reflect inputs about
///   180° to fold into `[0°, 180°]`.
/// * **span ≈ 360°**: full sweep, no implicit symmetry — wrap via
///   `rem_euclid(360°)`.
/// * **non-standard span**: leave the input as-is (modulo the `[first,
///   first + 360)` window) and let [`bracket_angle`] clamp at the
///   boundaries. The LM-63 spec doesn't define implicit symmetry for
///   arbitrary spans, so any extrapolation would be guesswork.
///
/// Folding inside [`IesProfile::candela_at`] (rather than only in the
/// GPU lookup-texture sampler) guarantees that
/// [`IesProfile::to_lookup_texture`] produces a periodic texture by
/// construction — column 0 and column `width − 1` agree because both
/// resolve to the same source sample after folding. This eliminates the
/// 360°-wrap discontinuity that would otherwise appear in the GPU
/// sampler for partial-span profiles, since the sampler wraps via
/// `rem_euclid(360°)` and assumes periodicity.
fn fold_horizontal_for_symmetry(angles: &[f32], h: f32) -> f32 {
    if angles.is_empty() {
        return h;
    }
    if angles.len() == 1 {
        return angles[0];
    }
    let first = angles[0];
    let last = angles[angles.len() - 1];
    let span = last - first;
    // Normalise into `[first, first + 360)` so reflections work in a
    // fixed coordinate system regardless of the file's chosen start
    // angle (the LM-63 spec lets a file start at any angle, though 0°
    // is overwhelmingly common in practice).
    let mut a = (h - first).rem_euclid(360.0);
    const EPS: f32 = 1e-3;
    if (span - 90.0).abs() < EPS {
        // 4-way (quadrant) symmetry: fold into `[0°, 90°]`.
        if a > 180.0 {
            a = 360.0 - a;
        }
        if a > 90.0 {
            a = 180.0 - a;
        }
    } else if (span - 180.0).abs() < EPS {
        // Bilateral symmetry: fold into `[0°, 180°]`.
        if a > 180.0 {
            a = 360.0 - a;
        }
    }
    // For span ≈ 360° or non-standard spans, leave `a` as the
    // wrap-normalised value. `bracket_angle` will clamp at the
    // boundaries when needed.
    a + first
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
    fn studio_preset_emits_sun_first_in_native_light_list() {
        // The native path tracer ingests `RenderLight`s directly via
        // `NativeLight::from_render_light`. The studio preset must keep
        // the sun as the first light so MIS-importance-sampling order is
        // deterministic across renders.
        let preset = LightingPreset::studio();
        let lights = preset.build_lights();
        assert!(matches!(lights[0], RenderLight::SunSky { .. }));
        // Studio has 3 accents + 1 sun.
        assert_eq!(lights.len(), 4);
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

    /// Fixture: an explicitly **asymmetric** Type-C IES distribution.
    /// 3 vertical × 5 horizontal angles, with distinct candela per
    /// horizontal slice at the equator (v = 90°). The H=0° and H=360°
    /// slices are identical (the period closure required by LM-63 for
    /// asymmetric distributions). The candela values per (H, V) are:
    ///
    /// ```text
    ///                 V=0°    V=90°   V=180°
    ///   H=0°           100     200      50
    ///   H=90°          100     300      50
    ///   H=180°         100     400      50
    ///   H=270°         100     500      50
    ///   H=360°         100     200      50   (= H=0°, period closure)
    /// ```
    ///
    /// IES candela order is H-outer, V-inner, so the candela table is
    /// emitted as five contiguous (V=0, V=90, V=180) triplets.
    const ASYMMETRIC_IES: &str = r#"IESNA:LM-63-2002
[TEST=Cognition AEC Studio asymmetric fixture]
[MANUFAC=ACME]
TILT=NONE
1 1000.0 1.0 3 5 1 2 0.0 0.0 0.0
1.0 1.0 100.0
0.0 90.0 180.0
0.0 90.0 180.0 270.0 360.0
100.0 200.0 50.0
100.0 300.0 50.0
100.0 400.0 50.0
100.0 500.0 50.0
100.0 200.0 50.0
"#;

    #[test]
    fn lookup_texture_sample_matches_bake_at_each_horizontal_angle() {
        // Regression: previously `IesLookupTexture::sample` mapped
        // `[0°, 360°)` onto `[0, width)` (circular convention), but the
        // bake stores its samples on `[0, width − 1]` (endpoint
        // inclusive — column `w-1` is exactly 360°). Sampling at a
        // horizontal angle that the bake stored exactly (e.g. 90°,
        // 180°) MUST return that stored candela byte-for-byte. Without
        // the fix, sampling at 90° on a 5-wide texture returns
        // `0.75 · cd(90°) + 0.25 · cd(180°)` — a blend that confuses
        // every column with its neighbour.
        let p = IesProfile::parse_ies(ASYMMETRIC_IES).unwrap();
        let tex = p.to_lookup_texture(5, 3);
        assert_eq!(tex.width, 5);
        assert_eq!(tex.height, 3);

        // Per the bake invariant, column `x` holds candela at
        // `x / (w - 1) · 360°`. At the equator (v = 90°), the
        // distinct cd values per horizontal slice MUST round-trip
        // exactly.
        let v_equator = 90.0;
        for (h_deg, expected_cd) in [
            (0.0_f32, 200.0_f32),
            (90.0, 300.0),
            (180.0, 400.0),
            (270.0, 500.0),
            (360.0, 200.0),
        ] {
            let got = tex.sample(v_equator, h_deg);
            assert!(
                (got - expected_cd).abs() < 1e-4,
                "sample at v={v_equator}, h={h_deg}: expected {expected_cd}, got {got}"
            );
        }
    }

    #[test]
    fn lookup_texture_sample_interpolates_between_adjacent_horizontal_columns() {
        // Halfway between H=90° (cd=300) and H=180° (cd=400) at the
        // equator should land on cd=350 once the endpoint-inclusive
        // mapping is in place. Under the old (buggy) circular
        // mapping, this would silently shift to ~362.5.
        let p = IesProfile::parse_ies(ASYMMETRIC_IES).unwrap();
        let tex = p.to_lookup_texture(5, 3);
        let got = tex.sample(90.0, 135.0);
        assert!(
            (got - 350.0).abs() < 1e-3,
            "expected midpoint cd=350.0 between H=90° and H=180°, got {got}"
        );
    }

    #[test]
    fn lookup_texture_sample_wraps_horizontal_modulo_360() {
        // 360° and 0° must map to the same column, and negative /
        // overshooting inputs (-45°, 405°) must wrap correctly through
        // `rem_euclid`. We check this at the equator on the asymmetric
        // fixture so it can catch a regression that swaps `rem_euclid`
        // for a plain modulo (which would map -45° to -45 and panic).
        let p = IesProfile::parse_ies(ASYMMETRIC_IES).unwrap();
        let tex = p.to_lookup_texture(5, 3);
        let v = 90.0;
        let at_0 = tex.sample(v, 0.0);
        let at_360 = tex.sample(v, 360.0);
        let at_minus_45 = tex.sample(v, -45.0);
        let at_315 = tex.sample(v, 315.0);
        let at_405 = tex.sample(v, 405.0);
        let at_45 = tex.sample(v, 45.0);
        assert!((at_0 - at_360).abs() < 1e-4);
        assert!((at_minus_45 - at_315).abs() < 1e-4);
        assert!((at_405 - at_45).abs() < 1e-4);
    }

    /// Quadrant-symmetric (LM-63 4-way) fixture: horizontal span 0°..=90°.
    /// Equator candela: cd(h=0°)=100, cd(h=45°)=200, cd(h=90°)=300.
    /// All other rows are zero (poles); only the equator is exercised
    /// in these tests.
    const QUADRANT_SYMMETRIC_IES: &str = r#"IESNA:LM-63-2002
[TEST=Cognition AEC Studio quadrant symmetry fixture]
TILT=NONE
1 1000.0 1.0 3 3 1 2 0.0 0.0 0.0
1.0 1.0 100.0
0.0 90.0 180.0
0.0 45.0 90.0
0.0 100.0 0.0
0.0 200.0 0.0
0.0 300.0 0.0
"#;

    /// Bilaterally-symmetric (LM-63 2-way / plane-symmetric) fixture:
    /// horizontal span 0°..=180°. Equator candela: cd(h=0°)=100,
    /// cd(h=90°)=200, cd(h=180°)=300.
    const BILATERAL_SYMMETRIC_IES: &str = r#"IESNA:LM-63-2002
[TEST=Cognition AEC Studio bilateral symmetry fixture]
TILT=NONE
1 1000.0 1.0 3 3 1 2 0.0 0.0 0.0
1.0 1.0 100.0
0.0 90.0 180.0
0.0 90.0 180.0
0.0 100.0 0.0
0.0 200.0 0.0
0.0 300.0 0.0
"#;

    #[test]
    fn candela_at_folds_quadrant_symmetric_profile_into_first_quadrant() {
        // Regression: previously `candela_at` clamped horizontal inputs
        // outside the sampled `[0°, 90°]` range to the boundary, so a
        // 4-way-symmetric file's cd(h=135°) would return cd(h=90°)
        // instead of cd(h=45°) (its true value by reflection symmetry).
        // The LM-63 spec defines 4-way symmetry: cd(a) = cd(180°−a) =
        // cd(180°+a) = cd(360°−a).
        let p = IesProfile::parse_ies(QUADRANT_SYMMETRIC_IES).unwrap();
        let v = 90.0;
        // Sampled values at the equator.
        assert!((p.candela_at(v, 0.0) - 100.0).abs() < 1e-4);
        assert!((p.candela_at(v, 45.0) - 200.0).abs() < 1e-4);
        assert!((p.candela_at(v, 90.0) - 300.0).abs() < 1e-4);
        // Reflection about 90° axis: cd(135°) = cd(180°-135°) = cd(45°).
        assert!((p.candela_at(v, 135.0) - 200.0).abs() < 1e-4);
        // Reflection about 180° axis: cd(225°) = cd(360°-225°) = cd(135°)
        // = cd(45°).
        assert!((p.candela_at(v, 225.0) - 200.0).abs() < 1e-4);
        // cd(270°) = cd(360°-270°) = cd(90°) = 300.
        assert!((p.candela_at(v, 270.0) - 300.0).abs() < 1e-4);
        // cd(315°) = cd(360°-315°) = cd(45°) = 200.
        assert!((p.candela_at(v, 315.0) - 200.0).abs() < 1e-4);
        // cd(360°) wraps to cd(0°) = 100.
        assert!((p.candela_at(v, 360.0) - 100.0).abs() < 1e-4);
        // cd(-45°) wraps to cd(315°) = cd(45°) = 200.
        assert!((p.candela_at(v, -45.0) - 200.0).abs() < 1e-4);
    }

    #[test]
    fn candela_at_folds_bilateral_symmetric_profile_about_180_plane() {
        // LM-63 bilateral symmetry: cd(a) = cd(360°−a) for any a, but
        // the input is folded into `[0°, 180°]` rather than `[0°, 90°]`.
        let p = IesProfile::parse_ies(BILATERAL_SYMMETRIC_IES).unwrap();
        let v = 90.0;
        // Sampled values at the equator.
        assert!((p.candela_at(v, 0.0) - 100.0).abs() < 1e-4);
        assert!((p.candela_at(v, 90.0) - 200.0).abs() < 1e-4);
        assert!((p.candela_at(v, 180.0) - 300.0).abs() < 1e-4);
        // cd(270°) reflects to cd(360°-270°) = cd(90°) = 200.
        assert!((p.candela_at(v, 270.0) - 200.0).abs() < 1e-4);
        // cd(225°) reflects to cd(360°-225°) = cd(135°) (interp
        // between h=90 cd=200 and h=180 cd=300 at t=0.5 → 250).
        assert!(
            (p.candela_at(v, 225.0) - 250.0).abs() < 1e-3,
            "cd(225°) bilateral-folded: got {}",
            p.candela_at(v, 225.0)
        );
        // cd(360°) wraps to cd(0°) = 100.
        assert!((p.candela_at(v, 360.0) - 100.0).abs() < 1e-4);
    }

    #[test]
    fn lookup_texture_periodic_at_360_wrap_for_quadrant_symmetric_profile() {
        // Critical: the GPU sampler wraps horizontal via `rem_euclid`,
        // so column 0 and column `width − 1` MUST hold the same value
        // or the sampler will see a discontinuity at the 360° wrap.
        // Before the LM-63 symmetry fold landed in `candela_at`, a
        // quadrant-symmetric (0°..=90°) profile baked into a wide
        // texture would have cd(360°) clamped to cd(90°)=300 in the
        // last column but cd(0°)=100 in the first column — a hard
        // step at the wrap boundary.
        let p = IesProfile::parse_ies(QUADRANT_SYMMETRIC_IES).unwrap();
        let tex = p.to_lookup_texture(13, 3);
        let last_col = tex.width as usize - 1;
        let w = tex.width as usize;
        // Periodicity invariant: column 0 (h=0°) == column `w-1` (h=360°).
        let v_equator_row = 1; // h=3 implies rows {0=pole, 1=equator, 2=pole}.
        let c0 = tex.candela[v_equator_row * w];
        let c_last = tex.candela[v_equator_row * w + last_col];
        assert!(
            (c0 - c_last).abs() < 1e-3,
            "bake not periodic: col 0 = {c0}, col {last_col} = {c_last}"
        );
        // And the sampler must be continuous across the wrap: at
        // equidistant offsets from the boundary, the sampled values
        // must agree (sampling 359.99° from below the wrap must equal
        // sampling 0.01° from above — both are ~0.0003 columns away
        // from the shared boundary column). Without the LM-63 fold,
        // 359.99° would sample near cd(90°)=300 while 0.01° samples
        // near cd(0°)=100 — a 200 cd hard step across an infinitesimal
        // input change.
        let just_below_360 = tex.sample(90.0, 359.99);
        let just_above_zero = tex.sample(90.0, 0.01);
        assert!(
            (just_below_360 - just_above_zero).abs() < 1e-3,
            "sampler discontinuous at 360° wrap: 359.99°={just_below_360}, 0.01°={just_above_zero}"
        );
    }

    #[test]
    fn lookup_texture_periodic_at_360_wrap_for_bilateral_symmetric_profile() {
        // Same periodicity invariant for the bilateral case. Without
        // the LM-63 fold, cd(360°) would clamp to cd(180°)=300 in the
        // last column but cd(0°)=100 in the first.
        let p = IesProfile::parse_ies(BILATERAL_SYMMETRIC_IES).unwrap();
        let tex = p.to_lookup_texture(13, 3);
        let last_col = tex.width as usize - 1;
        let w = tex.width as usize;
        let v_equator_row = 1;
        let c0 = tex.candela[v_equator_row * w];
        let c_last = tex.candela[v_equator_row * w + last_col];
        assert!(
            (c0 - c_last).abs() < 1e-3,
            "bake not periodic: col 0 = {c0}, col {last_col} = {c_last}"
        );
    }

    #[test]
    fn lookup_texture_sample_handles_width_one() {
        // Edge case: a rotationally-symmetric profile baked at width=1
        // (only one horizontal column = 0°/360°). Any horizontal input
        // must read from column 0 without indexing past the end. The
        // previous code's `(x0 + 1) % w` path also worked here, but
        // the fix's clamp-to-`w-1` path is the one being exercised.
        let p = IesProfile::parse_ies(SAMPLE_IES).unwrap();
        let tex = p.to_lookup_texture(1, 5);
        assert_eq!(tex.width, 1);
        // SAMPLE_IES has cd(v=0°) = 10.0.
        for h in [0.0_f32, 47.0, 180.0, 360.0, -90.0] {
            let got = tex.sample(0.0, h);
            assert!((got - 10.0).abs() < 1e-4, "h={h}: expected 10.0, got {got}");
        }
    }
}
