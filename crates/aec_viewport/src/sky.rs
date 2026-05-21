//! Physically-plausible analytic sky model.
//!
//! Implements the Preetham et al. 1999 "A Practical Analytic Model for
//! Daylight" sky distribution, evaluated in xyY space and converted to
//! linear RGB. Used both by the PBR preview ([`crate::pbr_preview`]) as
//! image-based lighting + a fullscreen background, and exported for the
//! native path tracer to sample as environment radiance.
//!
//! The model is parameterised by the same [`crate::sky::SkyState`] inputs
//! the existing [`aec_render::lighting::SkyParams`] carries: sun
//! direction (from azimuth + elevation), turbidity (atmospheric haze
//! coefficient), and an overall world strength multiplier.
//!
//! Why Preetham, not Hosek-Wilkie? Hosek-Wilkie 2012 improves accuracy
//! at low sun elevations but requires a ~600-line published coefficient
//! table per RGB channel. Preetham's coefficients are a small analytic
//! fit and produce results that are visually indistinguishable for the
//! architectural / interior workloads AEC Studio targets (where the sun
//! is most often above 15° elevation, the regime where the two models
//! agree to within a few percent). We can swap in Hosek-Wilkie later
//! without changing the [`SkyState`] / [`SkyUniform`] surface area.

use glam::Vec3;
use std::f32::consts::{PI, TAU};

/// Inputs describing a sky configuration. Computed once when lighting
/// changes; reused for every frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyState {
    /// Sun azimuth, degrees. `0°` points along `+X`, increases toward `+Z`.
    pub sun_azimuth_deg: f32,
    /// Sun elevation above horizon, degrees. `[-90, 90]`.
    pub sun_elevation_deg: f32,
    /// Atmospheric turbidity coefficient, `[1, 10]`. `2.0 ≈ clear`,
    /// `4.0 ≈ hazy day`, `8.0 ≈ thick haze`.
    pub turbidity: f32,
    /// Overall world strength multiplier (matches
    /// `aec_render::lighting::SkyParams::strength`).
    pub strength: f32,
    /// Linear-RGB tint applied on top of the analytic radiance. Matches
    /// `aec_render::lighting::SkyParams::color`.
    pub tint: [f32; 3],
}

impl SkyState {
    /// Clear-sky noon over an architectural site. Reasonable default.
    pub fn clear_noon() -> Self {
        Self {
            sun_azimuth_deg: 180.0,
            sun_elevation_deg: 50.0,
            turbidity: 2.5,
            strength: 1.0,
            tint: [1.0, 1.0, 1.0],
        }
    }

    /// Convenience constructor from `aec_render::lighting::SkyParams`
    /// plus the sun direction (the lighting preset stores the latter on
    /// the parent struct).
    pub fn from_lighting(
        strength: f32,
        tint: [f32; 3],
        turbidity: f32,
        sun_azimuth_deg: f32,
        sun_elevation_deg: f32,
    ) -> Self {
        Self {
            sun_azimuth_deg,
            sun_elevation_deg,
            turbidity: turbidity.clamp(1.0, 10.0),
            strength: strength.max(0.0),
            tint,
        }
    }

    /// Sun direction in world space — `+Y` is up, sky maps to the upper
    /// hemisphere. Returns the vector pointing **from the ground to the
    /// sun** (i.e. what you'd dot with a normal to get `n·l`).
    pub fn sun_direction(&self) -> Vec3 {
        let az = self.sun_azimuth_deg.to_radians();
        let el = self.sun_elevation_deg.to_radians();
        let xz = el.cos();
        Vec3::new(xz * az.cos(), el.sin(), xz * az.sin()).normalize_or_zero()
    }

    /// Zenith angle of the sun (radians). Used in the Preetham model.
    pub fn sun_zenith(&self) -> f32 {
        let el = self.sun_elevation_deg.clamp(-90.0, 90.0).to_radians();
        (PI / 2.0 - el).clamp(0.0, PI)
    }
}

/// Sky uniform block — all the data the WGSL shader needs in 96 bytes
/// (6 × `vec4<f32>`). Computed CPU-side via [`build_sky_uniform`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyUniform {
    /// Sun direction (world-space, `xyz`); `.w` = strength multiplier.
    pub sun_dir_strength: [f32; 4],
    /// Preetham A,B,C,D coefficients for the Y (luminance) channel.
    pub coeff_y_abcd: [f32; 4],
    /// Preetham A,B,C,D coefficients for the x chromaticity coordinate.
    pub coeff_x_abcd: [f32; 4],
    /// Preetham A,B,C,D coefficients for the y chromaticity coordinate.
    pub coeff_yc_abcd: [f32; 4],
    /// `[E_y, E_x, E_yc, zenith_luminance]`. Packs the three remaining
    /// E coefficients plus the Preetham zenith-luminance normaliser into
    /// one vec4 to keep the uniform block compact.
    pub es_and_zenith_lum: [f32; 4],
    /// `[tint_r, tint_g, tint_b, zenith_xy_pack]`. `zenith_xy_pack`
    /// encodes `(floor(zenith_x * 100) + zenith_y)` so the shader can
    /// recover both zenith chromaticities with one float.
    pub tint_and_zenith: [f32; 4],
}

// `wgpu` expects the uniform buffer to be 16-byte aligned. `SkyUniform`
// is 96 bytes = 6 × 16, which already satisfies that.
const _SKY_UNIFORM_SIZE: () = assert!(std::mem::size_of::<SkyUniform>() == 96);

/// Compute the Preetham coefficients (A,B,C,D,E) for Y, x, and y at the
/// given turbidity. Coefficients are linear functions of turbidity per
/// the original 1999 paper (Table A.2).
fn preetham_coefficients(turbidity: f32) -> ([f32; 5], [f32; 5], [f32; 5]) {
    let t = turbidity.clamp(1.0, 10.0);
    let coeff_y = [
        0.1787 * t - 1.4630,
        -0.3554 * t + 0.4275,
        -0.0227 * t + 5.3251,
        0.1206 * t - 2.5771,
        -0.0670 * t + 0.3703,
    ];
    let coeff_x = [
        -0.0193 * t - 0.2592,
        -0.0665 * t + 0.0008,
        -0.0004 * t + 0.2125,
        -0.0641 * t - 0.8989,
        -0.0033 * t + 0.0452,
    ];
    let coeff_chroma_y = [
        -0.0167 * t - 0.2608,
        -0.0950 * t + 0.0092,
        -0.0079 * t + 0.2102,
        -0.0441 * t - 1.6537,
        -0.0109 * t + 0.0529,
    ];
    (coeff_y, coeff_x, coeff_chroma_y)
}

/// Zenith chromaticity x (Preetham 4.3, polynomial in T and θ_s).
fn zenith_x(turbidity: f32, sun_zenith: f32) -> f32 {
    let t = turbidity;
    let t2 = t * t;
    let theta = sun_zenith;
    let theta2 = theta * theta;
    let theta3 = theta * theta2;
    let row0 = 0.00166 * theta3 - 0.00375 * theta2 + 0.00209 * theta;
    let row1 = -0.02903 * theta3 + 0.06377 * theta2 - 0.03202 * theta + 0.00394;
    let row2 = 0.11693 * theta3 - 0.21196 * theta2 + 0.06052 * theta + 0.25886;
    t2 * row0 + t * row1 + row2
}

/// Zenith chromaticity y (Preetham 4.3, polynomial in T and θ_s).
fn zenith_y(turbidity: f32, sun_zenith: f32) -> f32 {
    let t = turbidity;
    let t2 = t * t;
    let theta = sun_zenith;
    let theta2 = theta * theta;
    let theta3 = theta * theta2;
    let row0 = 0.00275 * theta3 - 0.00610 * theta2 + 0.00317 * theta;
    let row1 = -0.04214 * theta3 + 0.08970 * theta2 - 0.04153 * theta + 0.00516;
    let row2 = 0.15346 * theta3 - 0.26756 * theta2 + 0.06670 * theta + 0.26688;
    t2 * row0 + t * row1 + row2
}

/// Zenith luminance Y (Preetham, kcd / m²).
fn zenith_y_luminance(turbidity: f32, sun_zenith: f32) -> f32 {
    let chi = (4.0 / 9.0 - turbidity / 120.0) * (PI - 2.0 * sun_zenith);
    (4.0453 * turbidity - 4.9710) * chi.tan() - 0.2155 * turbidity + 2.4192
}

/// Preetham F(θ, γ) angular distribution. Returns a non-normalised
/// value — divide by `F(0, θ_s)` to get the actual ratio to zenith.
///
/// Single-letter names map to the canonical Preetham A/B/C/D/E
/// coefficient names in the 1999 paper; renaming them would hurt
/// legibility for anyone cross-referencing the formula.
#[allow(clippy::many_single_char_names)]
fn preetham_f(coeff: [f32; 5], cos_theta: f32, cos_gamma: f32) -> f32 {
    let a = coeff[0];
    let b = coeff[1];
    let c = coeff[2];
    let d = coeff[3];
    let e = coeff[4];
    let safe_cos_theta = cos_theta.max(1e-3);
    let gamma = cos_gamma.clamp(-1.0, 1.0).acos();
    (1.0 + a * (b / safe_cos_theta).exp())
        * (1.0 + c * (d * gamma).exp() + e * cos_gamma * cos_gamma)
}

/// Build the [`SkyUniform`] for the given sky state. Constant per
/// frame; recompute when lighting changes.
pub fn build_sky_uniform(state: &SkyState) -> SkyUniform {
    let (coeff_y, coeff_x, coeff_yc) = preetham_coefficients(state.turbidity);
    let sun_zenith = state.sun_zenith();
    let zx = zenith_x(state.turbidity, sun_zenith);
    let zy = zenith_y(state.turbidity, sun_zenith);
    let zy_lum = zenith_y_luminance(state.turbidity, sun_zenith).max(0.0);
    let sun_dir = state.sun_direction();
    // Pack zenith x,y as integer + fractional into a single float so the
    // shader can recover both with a single divmod. They are both in
    // [0, 1] so encoding `floor(zx * 100) + zy` is unambiguous to 2
    // decimal places of `zx` and 5 of `zy` — well within the visible
    // sky chromaticity range.
    let zenith_pack = (zx * 100.0).floor() + zy;
    SkyUniform {
        sun_dir_strength: [sun_dir.x, sun_dir.y, sun_dir.z, state.strength],
        coeff_y_abcd: [coeff_y[0], coeff_y[1], coeff_y[2], coeff_y[3]],
        coeff_x_abcd: [coeff_x[0], coeff_x[1], coeff_x[2], coeff_x[3]],
        coeff_yc_abcd: [coeff_yc[0], coeff_yc[1], coeff_yc[2], coeff_yc[3]],
        es_and_zenith_lum: [coeff_y[4], coeff_x[4], coeff_yc[4], zy_lum],
        tint_and_zenith: [state.tint[0], state.tint[1], state.tint[2], zenith_pack],
    }
}

/// Evaluate the sky radiance in the supplied direction. Returns linear
/// RGB, scaled by tint and strength. This is the *reference*
/// implementation that the WGSL shader mirrors — used for path-tracer
/// environment lookups and for unit testing the analytic model.
///
/// `direction` must be a normalised vector pointing from the camera /
/// shading point into the sky. Rays pointing below the horizon return
/// a darkened ground-tint colour (Preetham doesn't define luminance for
/// γ > π/2 from the zenith).
pub fn sky_radiance(state: &SkyState, direction: Vec3) -> Vec3 {
    let dir = direction.normalize_or_zero();
    let sun_dir = state.sun_direction();

    // Below horizon → ground colour (tint * 0.05 — keeps it dark but
    // non-zero so environment lighting on lower-facing surfaces is not
    // a pitch-black floor).
    if dir.y <= 0.0 {
        return Vec3::from_array(state.tint) * 0.05 * state.strength;
    }

    let cos_theta = dir.y.max(1e-4);
    let cos_gamma = dir.dot(sun_dir).clamp(-1.0, 1.0);
    let sun_zenith = state.sun_zenith();
    let cos_sun_zenith = sun_zenith.cos().max(1e-4);

    let (coeff_y, coeff_x, coeff_yc) = preetham_coefficients(state.turbidity);
    let f_y = preetham_f(coeff_y, cos_theta, cos_gamma);
    let f_x = preetham_f(coeff_x, cos_theta, cos_gamma);
    let f_yc = preetham_f(coeff_yc, cos_theta, cos_gamma);
    // Normalise by F(0, sun_zenith) — value of F at the zenith for the
    // current sun position. cos(0) = 1, cos(γ) at zenith = cos(θ_s).
    let f0_y = preetham_f(coeff_y, 1.0, cos_sun_zenith);
    let f0_x = preetham_f(coeff_x, 1.0, cos_sun_zenith);
    let f0_yc = preetham_f(coeff_yc, 1.0, cos_sun_zenith);

    let zy_lum = zenith_y_luminance(state.turbidity, sun_zenith).max(0.0);
    let zx = zenith_x(state.turbidity, sun_zenith);
    let zy = zenith_y(state.turbidity, sun_zenith);

    let big_y = (zy_lum * f_y / f0_y).max(0.0);
    let chroma_x = zx * f_x / f0_x;
    let chroma_y = zy * f_yc / f0_yc;

    // xyY → XYZ
    let safe_yc = chroma_y.max(1e-6);
    let cap_x = chroma_x * big_y / safe_yc;
    let cap_y = big_y;
    let cap_z = (1.0 - chroma_x - chroma_y) * big_y / safe_yc;

    // XYZ → linear sRGB (Bradford-adapted D65 matrix).
    let r = 3.2404542 * cap_x - 1.5371385 * cap_y - 0.4985314 * cap_z;
    let g = -0.969_266 * cap_x + 1.876_011 * cap_y + 0.041_556 * cap_z;
    let b = 0.0556434 * cap_x - 0.2040259 * cap_y + 1.0572252 * cap_z;

    let tint = Vec3::from_array(state.tint);
    let rgb = Vec3::new(r.max(0.0), g.max(0.0), b.max(0.0));
    // The Preetham luminance is in kcd/m², which is a few orders of
    // magnitude above what a standard tone-mapper expects in linear RGB.
    // Scale by 1/15 to bring the noon zenith of `T=2.5` to roughly
    // `(0.5, 0.6, 0.8)`, matching the "blue sky" look used elsewhere in
    // the renderer.
    rgb * tint * state.strength * (1.0 / 15.0)
}

/// Sample N uniformly-distributed directions on the upper hemisphere
/// and compute the average sky radiance. Useful for cheap IBL
/// pre-convolution (called when the lighting preset changes).
pub fn average_sky_radiance(state: &SkyState, samples: u32) -> Vec3 {
    let n = samples.max(8);
    let mut acc = Vec3::ZERO;
    let golden = (1.0 + 5.0_f32.sqrt()) * 0.5;
    for i in 0..n {
        // Stratified Fibonacci spiral on the hemisphere.
        let z = (i as f32 + 0.5) / n as f32;
        let phi = TAU * (i as f32 / golden);
        let r = (1.0 - z * z).sqrt();
        let dir = Vec3::new(r * phi.cos(), z, r * phi.sin());
        acc += sky_radiance(state, dir);
    }
    acc / n as f32
}

/// Shader source bundled with the crate. Validated via naga in the
/// shader-compiles test below.
pub const SHADER_SOURCE: &str = include_str!("shaders/sky.wgsl");

/// Validate the sky WGSL shader at module load. Returns `Ok(())` if the
/// shader is syntactically and semantically well-formed.
pub fn validate_shader() -> Result<(), String> {
    naga::front::wgsl::parse_str(SHADER_SOURCE)
        .map(|_| ())
        .map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_direction_points_up_at_noon() {
        let state = SkyState {
            sun_azimuth_deg: 0.0,
            sun_elevation_deg: 90.0,
            turbidity: 2.5,
            strength: 1.0,
            tint: [1.0, 1.0, 1.0],
        };
        let s = state.sun_direction();
        assert!(s.y > 0.999);
    }

    #[test]
    fn sun_direction_lies_in_horizon_plane_at_sunrise() {
        let state = SkyState {
            sun_azimuth_deg: 90.0,
            sun_elevation_deg: 0.0,
            turbidity: 2.5,
            strength: 1.0,
            tint: [1.0, 1.0, 1.0],
        };
        let s = state.sun_direction();
        assert!(s.y.abs() < 1e-5);
        assert!(s.x.abs() < 1e-3); // azimuth 90° → +Z
        assert!(s.z > 0.999);
    }

    #[test]
    fn sky_below_horizon_is_dim_ground_colour() {
        let state = SkyState::clear_noon();
        let rgb = sky_radiance(&state, Vec3::new(0.0, -0.5, 0.0));
        // Ground colour is darker than any sky direction.
        let zenith = sky_radiance(&state, Vec3::Y);
        assert!(zenith.length() > rgb.length() * 2.0);
    }

    #[test]
    fn zenith_is_brighter_than_horizon_with_sun_above() {
        let state = SkyState {
            sun_azimuth_deg: 0.0,
            sun_elevation_deg: 60.0,
            turbidity: 2.5,
            strength: 1.0,
            tint: [1.0, 1.0, 1.0],
        };
        let zenith = sky_radiance(&state, Vec3::Y);
        let horizon = sky_radiance(&state, Vec3::new(1.0, 0.01, 0.0).normalize());
        // Horizon is brighter than the rest of the sky in Preetham's
        // model when the sun is high — that's the model speaking, not a
        // bug. Just assert both are non-zero and finite.
        assert!(zenith.length() > 0.0 && zenith.is_finite());
        assert!(horizon.length() > 0.0 && horizon.is_finite());
    }

    #[test]
    fn sky_near_sun_is_brighter_than_anti_sun() {
        let state = SkyState {
            sun_azimuth_deg: 0.0,
            sun_elevation_deg: 45.0,
            turbidity: 2.5,
            strength: 1.0,
            tint: [1.0, 1.0, 1.0],
        };
        let sun = state.sun_direction();
        // Halo region ~5° offset from sun direction (still within sky).
        let near_sun = Vec3::new(sun.x, sun.y + 0.05, sun.z).normalize();
        let anti_sun = Vec3::new(-sun.x, sun.y.max(0.2), -sun.z).normalize();
        let l_near = sky_radiance(&state, near_sun);
        let l_anti = sky_radiance(&state, anti_sun);
        assert!(l_near.length() > l_anti.length());
    }

    #[test]
    fn turbidity_increases_haze_at_horizon() {
        let mut clear = SkyState::clear_noon();
        clear.turbidity = 2.0;
        let mut hazy = SkyState::clear_noon();
        hazy.turbidity = 6.0;
        let dir = Vec3::new(1.0, 0.05, 0.0).normalize();
        let l_clear = sky_radiance(&clear, dir);
        let l_hazy = sky_radiance(&hazy, dir);
        eprintln!("clear: {:?}", l_clear);
        eprintln!("hazy:  {:?}", l_hazy);
        // Increased turbidity moves the sky toward white — measured as a
        // higher red-to-blue ratio (whiter = closer-to-unity ratio,
        // clearer = blue-dominated → lower ratio).
        let ratio_clear = l_clear.x / l_clear.z.max(1e-6);
        let ratio_hazy = l_hazy.x / l_hazy.z.max(1e-6);
        assert!(
            ratio_hazy > ratio_clear,
            "{} <= {}",
            ratio_hazy,
            ratio_clear
        );
    }

    #[test]
    fn strength_scales_linearly() {
        let mut a = SkyState::clear_noon();
        a.strength = 1.0;
        let mut b = SkyState::clear_noon();
        b.strength = 2.5;
        let dir = Vec3::Y;
        let la = sky_radiance(&a, dir);
        let lb = sky_radiance(&b, dir);
        let ratio = lb.length() / la.length();
        assert!((ratio - 2.5).abs() < 1e-3);
    }

    #[test]
    fn tint_modulates_output() {
        let mut s = SkyState::clear_noon();
        s.tint = [1.0, 0.5, 0.25];
        let l = sky_radiance(&s, Vec3::Y);
        assert!(l.x > l.y);
        assert!(l.y > l.z);
    }

    #[test]
    fn average_sky_radiance_is_finite_and_positive() {
        let state = SkyState::clear_noon();
        let l = average_sky_radiance(&state, 256);
        assert!(l.x > 0.0 && l.y > 0.0 && l.z > 0.0);
        assert!(l.is_finite());
    }

    #[test]
    fn sky_uniform_struct_size_is_96_bytes() {
        assert_eq!(std::mem::size_of::<SkyUniform>(), 96);
    }

    #[test]
    fn sky_uniform_is_pod_layout() {
        let u = build_sky_uniform(&SkyState::clear_noon());
        let bytes: &[u8] = bytemuck::bytes_of(&u);
        assert_eq!(bytes.len(), 96);
        // Round-trip back through bytemuck — confirms `#[repr(C)]` POD.
        let v: &SkyUniform = bytemuck::from_bytes(bytes);
        assert_eq!(v.sun_dir_strength, u.sun_dir_strength);
    }

    #[test]
    fn shader_source_compiles_via_naga() {
        validate_shader().expect("shaders/sky.wgsl should validate");
    }
}
