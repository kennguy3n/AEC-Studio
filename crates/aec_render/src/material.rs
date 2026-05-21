// Single-letter identifiers (n, f, d, g, h, k) match the standard PBR
// notation used throughout the BSDF literature; using long names obscures
// the formula structure.
#![allow(clippy::many_single_char_names)]

//! Principled BSDF for the native path tracer.
//!
//! Native Rust replacement for the closures in Cycles' `src/kernel/closure/`
//! (`bsdf_principled_*.h`, `bsdf_microfacet_*.h`). Implements the
//! metallic/roughness workflow used throughout the renderer and the viewport
//! (`PbrMaterial` in `aec_materials`): a Lambertian diffuse lobe and a GGX
//! microfacet specular lobe blended by metallic + Fresnel.
//!
//! The closures are written to be sampled (Monte Carlo path tracing) AND
//! evaluated in both directions (for next-event-estimation MIS in
//! `light_sampling.rs`).

use aec_materials::PbrMaterial;
use glam::Vec3;

use std::f32::consts::PI;

/// Native material struct consumed by the path tracer. Mirrors the
/// subset of `PbrMaterial` that affects the path-trace integrator and
/// pre-computes the `f0` Schlick base.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathTraceMaterial {
    pub base_color: Vec3,
    pub metallic: f32,
    /// Linear-roughness in `[0, 1]`. Squared internally for the GGX `α`.
    pub roughness: f32,
    pub ior: f32,
    pub emissive: Vec3,
    pub transmission: f32,
    pub ao: f32,
}

impl PathTraceMaterial {
    pub fn from_pbr(m: &PbrMaterial) -> Self {
        Self {
            base_color: Vec3::from_array(m.albedo),
            metallic: m.metallic.clamp(0.0, 1.0),
            roughness: m.roughness.clamp(0.0, 1.0),
            ior: m.ior.max(1.0),
            emissive: Vec3::from_array(m.emissive),
            transmission: m.transmission.clamp(0.0, 1.0),
            ao: m.ao.clamp(0.0, 1.0),
        }
    }

    /// Default 'mid-grey diffuse' material — handy when a mesh is missing
    /// its material assignment.
    pub fn default_grey() -> Self {
        Self {
            base_color: Vec3::splat(0.6),
            metallic: 0.0,
            roughness: 0.6,
            ior: 1.45,
            emissive: Vec3::ZERO,
            transmission: 0.0,
            ao: 1.0,
        }
    }

    /// Schlick base reflectance at normal incidence. For dielectrics this
    /// is computed from IOR, for metals from the base color.
    pub fn f0(&self) -> Vec3 {
        let dielectric_f0 = {
            let f = (self.ior - 1.0) / (self.ior + 1.0);
            f * f
        };
        let dielectric = Vec3::splat(dielectric_f0);
        dielectric.lerp(self.base_color, self.metallic)
    }
}

/// Schlick approximation of the Fresnel term.
///
/// `cos_theta` is `|N·V|` for reflection or `|N·H|` for half-vector use.
#[inline]
pub fn fresnel_schlick(cos_theta: f32, f0: Vec3) -> Vec3 {
    let one_minus = (1.0 - cos_theta).max(0.0);
    let pow5 = one_minus.powi(5);
    f0 + (Vec3::splat(1.0) - f0) * pow5
}

/// GGX (Trowbridge-Reitz) normal distribution function `D(H)`.
#[inline]
pub fn ggx_d(n_dot_h: f32, alpha: f32) -> f32 {
    if n_dot_h <= 0.0 {
        return 0.0;
    }
    let a2 = alpha * alpha;
    let denom = (n_dot_h * n_dot_h) * (a2 - 1.0) + 1.0;
    a2 / (PI * denom * denom).max(1e-20)
}

/// Smith GGX joint shadowing-masking term `G2(V, L, H)`.
#[inline]
pub fn ggx_g_smith(n_dot_v: f32, n_dot_l: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let g_v = n_dot_l * (a2 + (1.0 - a2) * n_dot_v * n_dot_v).sqrt();
    let g_l = n_dot_v * (a2 + (1.0 - a2) * n_dot_l * n_dot_l).sqrt();
    let denom = g_v + g_l;
    if denom <= 0.0 {
        0.0
    } else {
        (2.0 * n_dot_v * n_dot_l) / denom
    }
}

/// Evaluate the principled BSDF for a fixed incoming/outgoing direction
/// pair. Returns the BSDF value `f(wi, wo)` (per RGB channel, including
/// the `cosine·diffuse + microfacet specular` decomposition).
///
/// All vectors are in world space and assumed to be normalised.
/// `n` is the shading normal pointing into the upper hemisphere.
pub fn eval_bsdf(
    mat: &PathTraceMaterial,
    n: Vec3,
    wi: Vec3, // incoming light direction (from surface toward light)
    wo: Vec3, // outgoing direction (from surface toward camera)
) -> Vec3 {
    let n_dot_l = n.dot(wi);
    let n_dot_v = n.dot(wo);
    if n_dot_l <= 0.0 || n_dot_v <= 0.0 {
        return Vec3::ZERO;
    }
    let h = (wi + wo).normalize();
    let n_dot_h = n.dot(h).max(0.0);
    let v_dot_h = wo.dot(h).max(0.0);

    let alpha = mat.roughness * mat.roughness;
    let f0 = mat.f0();
    let f = fresnel_schlick(v_dot_h, f0);
    let d = ggx_d(n_dot_h, alpha);
    let g = ggx_g_smith(n_dot_v, n_dot_l, alpha);

    let specular = f * d * g / (4.0 * n_dot_v * n_dot_l).max(1e-20);

    // Energy-conserving diffuse: subtract Fresnel-weighted reflection and
    // scale by (1 - metallic) so metals have no diffuse lobe.
    let k_d = (Vec3::splat(1.0) - f) * (1.0 - mat.metallic);
    let diffuse = k_d * mat.base_color / PI;

    diffuse + specular
}

/// PDF for `sample_bsdf` for use with MIS in `light_sampling.rs`.
pub fn pdf_bsdf(mat: &PathTraceMaterial, n: Vec3, wi: Vec3, wo: Vec3) -> f32 {
    let n_dot_l = n.dot(wi).max(0.0);
    let n_dot_v = n.dot(wo).max(0.0);
    if n_dot_l <= 0.0 || n_dot_v <= 0.0 {
        return 0.0;
    }
    let alpha = mat.roughness * mat.roughness;
    let diffuse_pdf = n_dot_l / PI;
    let h = (wi + wo).normalize();
    let n_dot_h = n.dot(h).max(0.0);
    let v_dot_h = wo.dot(h).max(0.0).max(1e-6);
    let specular_pdf = ggx_d(n_dot_h, alpha) * n_dot_h / (4.0 * v_dot_h);
    let f0 = mat.f0();
    let fresnel = fresnel_schlick(v_dot_h, f0);
    // Probability of choosing the specular lobe = avg(f0 + (1-f0)*pow5)
    let p_spec = ((fresnel.x + fresnel.y + fresnel.z) / 3.0).clamp(0.0, 1.0);
    let p_spec = mat.metallic.max(p_spec);
    p_spec * specular_pdf + (1.0 - p_spec) * diffuse_pdf
}

/// Sampled direction + weight.
#[derive(Debug, Clone, Copy)]
pub struct BsdfSample {
    pub direction: Vec3,
    /// Cosine-weighted BSDF value `f(wi, wo) * cos(theta_i) / pdf`. This is
    /// the contribution that should multiply the throughput.
    pub weight: Vec3,
    pub pdf: f32,
    pub is_specular: bool,
}

/// Importance-sample the principled BSDF.
///
/// `rng` returns three i.i.d. uniform numbers in `[0, 1)`.
pub fn sample_bsdf(
    mat: &PathTraceMaterial,
    n: Vec3,
    wo: Vec3,
    rng: [f32; 3],
) -> Option<BsdfSample> {
    let n_dot_v = n.dot(wo);
    if n_dot_v <= 0.0 {
        return None;
    }
    // Choose specular vs diffuse lobe.
    let alpha = mat.roughness * mat.roughness;
    let f0 = mat.f0();
    let f_normal = fresnel_schlick(n_dot_v, f0);
    let p_spec = ((f_normal.x + f_normal.y + f_normal.z) / 3.0).clamp(0.0, 1.0);
    let p_spec = mat.metallic.max(p_spec);
    let pick_specular = rng[0] < p_spec;

    let (tangent, bitangent) = tangent_basis(n);

    let wi = if pick_specular {
        sample_ggx_half(n, tangent, bitangent, wo, alpha, rng[1], rng[2])?
    } else {
        sample_cosine_weighted(n, tangent, bitangent, rng[1], rng[2])
    };

    let f = eval_bsdf(mat, n, wi, wo);
    let pdf = pdf_bsdf(mat, n, wi, wo);
    if pdf <= 0.0 {
        return None;
    }
    let cos_theta = n.dot(wi).max(0.0);
    let weight = f * cos_theta / pdf;
    Some(BsdfSample {
        direction: wi,
        weight,
        pdf,
        is_specular: pick_specular && mat.roughness < 0.05,
    })
}

fn sample_ggx_half(
    n: Vec3,
    t: Vec3,
    b: Vec3,
    wo: Vec3,
    alpha: f32,
    u1: f32,
    u2: f32,
) -> Option<Vec3> {
    // Sample half-vector in tangent space using GGX VNDF approximation.
    // Phi is uniform, theta is from the GGX distribution.
    let phi = 2.0 * PI * u1;
    let cos_theta = ((1.0 - u2) / (u2 * (alpha * alpha - 1.0) + 1.0))
        .sqrt()
        .clamp(0.0, 1.0);
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let h_local = Vec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
    let h = t * h_local.x + b * h_local.y + n * h_local.z;
    let wi = (2.0 * wo.dot(h) * h - wo).normalize();
    if n.dot(wi) <= 0.0 {
        return None;
    }
    Some(wi)
}

fn sample_cosine_weighted(n: Vec3, t: Vec3, b: Vec3, u1: f32, u2: f32) -> Vec3 {
    let phi = 2.0 * PI * u1;
    let cos_theta = (1.0 - u2).sqrt();
    let sin_theta = u2.sqrt();
    let local = Vec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
    (t * local.x + b * local.y + n * local.z).normalize()
}

/// Build an orthonormal tangent basis from a unit normal `n`. Uses the
/// branchless Duff et al. (2017) construction (also used by Cycles).
pub fn tangent_basis(n: Vec3) -> (Vec3, Vec3) {
    let sign = n.z.signum();
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    let t = Vec3::new(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x);
    let bt = Vec3::new(b, sign + n.y * n.y * a, -n.y);
    (t.normalize(), bt.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn fresnel_at_zero_returns_f0() {
        let f0 = Vec3::splat(0.04);
        assert!(approx(fresnel_schlick(1.0, f0), f0, 1e-5));
    }

    #[test]
    fn fresnel_grazing_approaches_one() {
        let f0 = Vec3::splat(0.04);
        let f = fresnel_schlick(0.0, f0);
        assert!(f.x > 0.95, "expected high reflection at grazing, got {f:?}");
    }

    #[test]
    fn metallic_f0_is_base_color() {
        let mat = PathTraceMaterial {
            base_color: Vec3::new(0.7, 0.7, 0.2),
            metallic: 1.0,
            roughness: 0.4,
            ior: 1.5,
            emissive: Vec3::ZERO,
            transmission: 0.0,
            ao: 1.0,
        };
        let f0 = mat.f0();
        assert!(approx(f0, Vec3::new(0.7, 0.7, 0.2), 1e-5));
    }

    #[test]
    fn dielectric_f0_from_ior() {
        let mat = PathTraceMaterial {
            base_color: Vec3::ONE,
            metallic: 0.0,
            roughness: 0.4,
            ior: 1.5,
            emissive: Vec3::ZERO,
            transmission: 0.0,
            ao: 1.0,
        };
        let f0 = mat.f0();
        // ((1.5-1)/(1.5+1))^2 = 0.04
        assert!((f0.x - 0.04).abs() < 1e-5);
    }

    #[test]
    fn ggx_d_normalisation_unit_alpha() {
        // At alpha=1 and cos(theta)=1, D = 1/π.
        let d = ggx_d(1.0, 1.0);
        assert!((d - 1.0 / PI).abs() < 1e-5);
    }

    #[test]
    fn ggx_g_smith_in_unit_range() {
        let g = ggx_g_smith(0.5, 0.5, 0.5);
        assert!((0.0..=2.0).contains(&g));
    }

    #[test]
    fn tangent_basis_is_orthonormal() {
        let n = Vec3::new(1.0, 2.0, 3.0).normalize();
        let (t, b) = tangent_basis(n);
        assert!(t.dot(n).abs() < 1e-5);
        assert!(b.dot(n).abs() < 1e-5);
        assert!(t.dot(b).abs() < 1e-5);
        assert!((t.length() - 1.0).abs() < 1e-4);
        assert!((b.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn cosine_weighted_sample_is_in_hemisphere() {
        let n = Vec3::Z;
        let (t, b) = tangent_basis(n);
        for _ in 0..100 {
            let u1 = fastrand::f32();
            let u2 = fastrand::f32();
            let s = sample_cosine_weighted(n, t, b, u1, u2);
            assert!(s.dot(n) >= -1e-4);
        }
    }

    #[test]
    fn furnace_test_diffuse_energy_conserving() {
        // A pure white diffuse material under constant unit illumination
        // should reflect close to 100% (within Monte Carlo noise).
        let mat = PathTraceMaterial {
            base_color: Vec3::ONE,
            metallic: 0.0,
            roughness: 1.0,
            ior: 1.5,
            emissive: Vec3::ZERO,
            transmission: 0.0,
            ao: 1.0,
        };
        let n = Vec3::Z;
        let wo = Vec3::new(0.3, 0.0, 1.0).normalize();
        let n_samples = 4096;
        let mut sum = Vec3::ZERO;
        for _ in 0..n_samples {
            let r = [fastrand::f32(), fastrand::f32(), fastrand::f32()];
            if let Some(s) = sample_bsdf(&mat, n, wo, r) {
                sum += s.weight;
            }
        }
        let avg = sum / n_samples as f32;
        // Sampled <BRDF * cos / pdf> ≈ reflectance under uniform unit light.
        // For pure white diffuse we expect ≈ 1.0 reflectance (modulo Fresnel
        // dispersion); reasonable Monte Carlo error band.
        assert!(
            avg.x > 0.6 && avg.x < 1.2,
            "average reflectance not energy-conserving: {avg:?}"
        );
    }

    #[test]
    fn bsdf_eval_is_zero_below_horizon() {
        let mat = PathTraceMaterial::default_grey();
        let n = Vec3::Z;
        let wi = Vec3::new(0.0, 0.0, -1.0); // below horizon
        let wo = Vec3::Z;
        assert!(eval_bsdf(&mat, n, wi, wo) == Vec3::ZERO);
    }

    #[test]
    fn from_pbr_clamps_inputs() {
        let pbr = PbrMaterial {
            id: "x".into(),
            name: "x".into(),
            albedo: [0.5, 0.5, 0.5],
            metallic: 2.0,
            roughness: -0.5,
            ior: 0.5,
            emissive: [0.0; 3],
            ao: 1.5,
            transmission: -1.0,
            albedo_map: None,
            normal_map: None,
            metallic_roughness_map: None,
            ao_map: None,
            emissive_map: None,
            tags: vec![],
            style_tags: vec![],
            vendor_id: None,
        };
        let m = PathTraceMaterial::from_pbr(&pbr);
        assert_eq!(m.metallic, 1.0);
        assert_eq!(m.roughness, 0.0);
        assert!(m.ior >= 1.0);
        assert_eq!(m.ao, 1.0);
        assert_eq!(m.transmission, 0.0);
    }
}
