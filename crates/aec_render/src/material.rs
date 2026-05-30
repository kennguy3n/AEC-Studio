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
use glam::{Vec2, Vec3, Vec4};

use std::f32::consts::PI;

use crate::texture::{bilinear_sample, MaterialTextureBindings, TextureAtlas};

/// Native material struct consumed by the path tracer. Mirrors the
/// subset of `PbrMaterial` that affects the path-trace integrator and
/// pre-computes the `f0` Schlick base.
///
/// The optional `texture_bindings` field carries indices into a
/// [`TextureAtlas`] for the four PBR slots (albedo, normal, packed
/// metallic-roughness, emissive). When `None`, the renderer uses the
/// flat `base_color` / `metallic` / `roughness` / `emissive` fields.
/// When `Some`, [`sample_textured_material`] resolves the slots and
/// returns a per-pixel [`PathTraceMaterial`] (with `texture_bindings`
/// cleared) that the BSDF evaluators can use directly — the bindings
/// are pinned to the triangle for the lifetime of the shading hit, so
/// resampling does not happen inside the BSDF sample/eval/pdf loop.
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
    /// Optional texture bindings — `None` when the material is
    /// flat-shaded, `Some` when one or more slots are textured.
    pub texture_bindings: Option<MaterialTextureBindings>,
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
            texture_bindings: None,
        }
    }

    /// Construct a [`PathTraceMaterial`] from a [`PbrMaterial`] plus a
    /// pre-resolved set of [`MaterialTextureBindings`] (typically
    /// produced by [`TextureAtlas::from_material_library`]). Pass
    /// [`MaterialTextureBindings::default`] when none of the maps were
    /// resolvable — the renderer will treat the material as flat.
    pub fn from_pbr_with_textures(m: &PbrMaterial, bindings: MaterialTextureBindings) -> Self {
        let mut out = Self::from_pbr(m);
        if bindings.albedo.is_some()
            || bindings.normal.is_some()
            || bindings.metallic_roughness.is_some()
            || bindings.emissive.is_some()
        {
            out.texture_bindings = Some(bindings);
        }
        out
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
            texture_bindings: None,
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

/// Resolve the texture-bound fields of `mat` at UV coordinate `uv`,
/// returning a new [`PathTraceMaterial`] with the textured channels
/// folded into the flat fields and `texture_bindings` cleared.
///
/// glTF / `KHR_materials_pbrSpecularGlossiness` packed-map convention:
/// the metallic-roughness texture stores `roughness` in the GREEN
/// channel and `metallic` in the BLUE channel; RED is unused. Albedo
/// and emissive maps are multiplied with the base scalar (matches the
/// glTF / Cycles convention so a white texture acts as identity).
///
/// If `mat.texture_bindings == None` this function is a no-op (returns
/// `*mat` unchanged) and skips the atlas lookup entirely.
pub fn sample_textured_material(
    mat: &PathTraceMaterial,
    atlas: &TextureAtlas,
    uv: Vec2,
) -> PathTraceMaterial {
    let Some(bindings) = mat.texture_bindings else {
        return *mat;
    };
    let mut out = *mat;
    let uv_arr = [uv.x, uv.y];
    if let Some(tex_id) = bindings.albedo {
        let s: Vec4 = bilinear_sample(atlas, tex_id, uv_arr, 0.0);
        out.base_color = mat.base_color * Vec3::new(s.x, s.y, s.z);
    }
    if let Some(tex_id) = bindings.metallic_roughness {
        let s: Vec4 = bilinear_sample(atlas, tex_id, uv_arr, 0.0);
        out.roughness = (mat.roughness * s.y).clamp(0.0, 1.0);
        out.metallic = (mat.metallic * s.z).clamp(0.0, 1.0);
    }
    if let Some(tex_id) = bindings.emissive {
        let s: Vec4 = bilinear_sample(atlas, tex_id, uv_arr, 0.0);
        // glTF / Cycles convention: emissive textures multiply with
        // the scalar `emissive_factor` so a white texel acts as
        // identity and a black texel zeroes the channel. Using `+`
        // here would silently energise materials with `mat.emissive
        // == Vec3::ZERO` (the default) — every textured surface
        // would glow even when `emissive_factor` says otherwise —
        // and would push hot pixels in white-texture areas above the
        // intended factor, breaking the doc-comment contract above
        // ("emissive maps are multiplied with the base scalar").
        out.emissive = mat.emissive * Vec3::new(s.x, s.y, s.z);
    }
    out.texture_bindings = None;
    out
}

/// Apply a normal-map perturbation to the shading normal `n` using the
/// triangle's tangent / bitangent basis. Returns the perturbed normal
/// (re-normalised). When the material has no normal map, returns `n`
/// unchanged.
///
/// The normal map is expected to be in tangent space with the standard
/// glTF / OpenGL `[0, 1]` → `[-1, 1]` remapping (`n = 2·sample - 1`).
pub fn sample_normal_map(
    bindings: Option<&MaterialTextureBindings>,
    atlas: &TextureAtlas,
    uv: Vec2,
    n: Vec3,
    t: Vec3,
    b: Vec3,
) -> Vec3 {
    let Some(b_set) = bindings else { return n };
    let Some(tex_id) = b_set.normal else { return n };
    let s = bilinear_sample(atlas, tex_id, [uv.x, uv.y], 0.0);
    // The sampler returns linear-space RGB in [0, 1]. Map back to the
    // signed tangent-space normal in [-1, 1].
    let nx = s.x * 2.0 - 1.0;
    let ny = s.y * 2.0 - 1.0;
    let nz = (s.z * 2.0 - 1.0).max(0.0);
    let perturbed = (t * nx + b * ny + n * nz).normalize_or_zero();
    if perturbed.length_squared() < 1e-6 {
        n
    } else {
        perturbed
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

/// Compute the refracted direction using Snell's law.
///
/// `wo` is the outgoing view direction (from surface toward camera).
/// `n` is the shading normal on the same side as `wo`. `eta` is the
/// ratio `n_outside / n_inside`. Returns `None` when total internal
/// reflection (TIR) occurs.
#[inline]
pub fn refract_snell(wo: Vec3, n: Vec3, eta: f32) -> Option<Vec3> {
    let cos_i = n.dot(wo).min(1.0);
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i);
    if sin2_t > 1.0 {
        return None; // TIR
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    Some(-eta * wo + (eta * cos_i - cos_t) * n)
}

/// Probability of choosing the transmission lobe. Metals don't transmit;
/// the factor `(1 - metallic)` zeroes this out for metals. This is the
/// same weighting used by Cycles' principled BSDF.
#[inline]
pub fn transmission_weight(mat: &PathTraceMaterial) -> f32 {
    mat.transmission * (1.0 - mat.metallic)
}

/// Evaluate the principled BSDF for a fixed incoming/outgoing direction
/// pair. Returns the BSDF value `f(wi, wo)` (per RGB channel, including
/// the `cosine·diffuse + microfacet specular` decomposition plus a
/// transmission term for glass / water).
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
    let p_trans = transmission_weight(mat);

    // Transmission lobe: wi is on the opposite hemisphere from wo.
    if p_trans > 0.0 && n_dot_l < 0.0 && n_dot_v > 0.0 {
        // delta-like transmission; for a rough glass this would be a
        // GGX microfacet BTDF but the typical architectural use-case
        // (clear glass) is effectively smooth. We return a constant
        // that integrates to the correct energy after dividing by the
        // transmission pdf below.
        let alpha = mat.roughness * mat.roughness;
        let eta = if n_dot_v > 0.0 {
            1.0 / mat.ior
        } else {
            mat.ior
        };
        // Rough BTDF: Cook-Torrance microfacet with half vector for
        // refractive interface. For near-smooth glass this collapses
        // toward a delta function, but the Monte Carlo weights remain
        // correct.
        let ht = -(eta * wi + wo).normalize();
        let n_dot_h = n.dot(ht).abs();
        let v_dot_h = wo.dot(ht).abs();
        let l_dot_h = wi.dot(ht).abs();
        let f0 = mat.f0();
        let f_r = fresnel_schlick(v_dot_h, f0);
        let t_frac = Vec3::ONE - f_r; // transmitted fraction
        let d = ggx_d(n_dot_h, alpha.max(0.001));
        let g = ggx_g_smith(n_dot_v.abs(), n_dot_l.abs(), alpha.max(0.001));
        let denom = (eta * l_dot_h + v_dot_h).powi(2);
        if denom < 1e-20 {
            return Vec3::ZERO;
        }
        let btdf = t_frac
            * mat.base_color
            * d
            * g
            * (v_dot_h * l_dot_h / (n_dot_v.abs() * denom).max(1e-20));
        return btdf;
    }

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
    // scale by (1 - metallic) so metals have no diffuse lobe. Also
    // subtract the transmission weight so energy is conserved.
    let k_d = (Vec3::splat(1.0) - f) * (1.0 - mat.metallic) * (1.0 - p_trans);
    let diffuse = k_d * mat.base_color / PI;

    diffuse + specular
}

/// PDF for `sample_bsdf` for use with MIS in `light_sampling.rs`.
///
/// The combined PDF mixes diffuse, specular, and (optionally)
/// transmission lobes with the same weights that `sample_bsdf` uses
/// to pick between them.
pub fn pdf_bsdf(mat: &PathTraceMaterial, n: Vec3, wi: Vec3, wo: Vec3) -> f32 {
    let n_dot_l = n.dot(wi);
    let n_dot_v = n.dot(wo);
    let p_trans = transmission_weight(mat);

    // Transmitted direction: wi is on the opposite side of the surface.
    if p_trans > 0.0 && n_dot_l < 0.0 && n_dot_v > 0.0 {
        // For near-smooth glass the transmission pdf is concentrated
        // around the refracted direction. We approximate via the BTDF
        // microfacet half-vector GGX density.
        let alpha = (mat.roughness * mat.roughness).max(0.001);
        let eta = 1.0 / mat.ior;
        let ht = -(eta * wi + wo).normalize();
        let n_dot_h = n.dot(ht).abs();
        let v_dot_h = wo.dot(ht).abs().max(1e-6);
        let l_dot_h = wi.dot(ht).abs().max(1e-6);
        let denom = (eta * l_dot_h + v_dot_h).powi(2);
        if denom < 1e-20 {
            return 0.0;
        }
        let trans_pdf = ggx_d(n_dot_h, alpha) * n_dot_h * l_dot_h / denom;
        return p_trans * trans_pdf;
    }

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
    let p_spec_raw = ((fresnel.x + fresnel.y + fresnel.z) / 3.0).clamp(0.0, 1.0);
    let p_spec_raw = mat.metallic.max(p_spec_raw);
    // Re-normalise probabilities after removing the transmission weight.
    let reflect_budget = 1.0 - p_trans;
    let p_spec = p_spec_raw * reflect_budget;
    let p_diff = (1.0 - p_spec_raw) * reflect_budget;
    p_spec * specular_pdf + p_diff * diffuse_pdf
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
    /// Whether this sample was drawn from the transmission lobe. The
    /// path tracer uses this to offset the next-ray origin to the
    /// **opposite** side of the surface instead of the same side.
    pub is_transmission: bool,
}

/// Importance-sample the principled BSDF.
///
/// `rng` is four i.i.d. uniform numbers in `[0, 1)`. The fourth
/// channel is consumed only by the transmission lobe (Fresnel
/// Russian-roulette between microfacet reflection and refraction);
/// the diffuse and specular lobes ignore it. A fourth sample is
/// required — not a third one reused — because using the same
/// scalar as both `u2` for the GGX half-vector azimuth and as the
/// threshold for the Fresnel split produces a deterministic
/// coupling between half-vector orientation and reflect-vs-refract
/// outcome, visibly biasing roughened glass renders (energy
/// stripes along one diagonal of the lobe). Allocate the fourth
/// sample from the same RNG you draw the first three from — see
/// `path_trace.rs`'s `[rng.f32(); 4]` call for the canonical
/// pattern.
pub fn sample_bsdf(
    mat: &PathTraceMaterial,
    n: Vec3,
    wo: Vec3,
    rng: [f32; 4],
) -> Option<BsdfSample> {
    let n_dot_v = n.dot(wo);
    if n_dot_v <= 0.0 {
        return None;
    }
    let alpha = mat.roughness * mat.roughness;
    let f0 = mat.f0();
    let f_normal = fresnel_schlick(n_dot_v, f0);
    let p_spec_raw = ((f_normal.x + f_normal.y + f_normal.z) / 3.0).clamp(0.0, 1.0);
    let p_spec_raw = mat.metallic.max(p_spec_raw);
    let p_trans = transmission_weight(mat);

    // Decide between three lobes. `r` partitions the unit interval as
    // [0, p_trans) → transmission, [p_trans, p_trans + p_spec) →
    // specular, the rest → diffuse.
    let r = rng[0];
    let p_spec = p_spec_raw * (1.0 - p_trans);
    let pick_transmission = r < p_trans;
    let pick_specular = !pick_transmission && r < p_trans + p_spec;

    let (tangent, bitangent) = tangent_basis(n);

    if pick_transmission {
        // Sample microfacet half-vector + refract through it. For
        // smooth glass this collapses to the geometric refraction
        // direction.
        let alpha_t = alpha.max(0.001);
        let h_world = sample_ggx_half_unconditional(n, tangent, bitangent, alpha_t, rng[1], rng[2]);
        let v_dot_h = wo.dot(h_world).max(0.0);
        let f_r = fresnel_schlick(v_dot_h, f0);
        let f_r_avg = ((f_r.x + f_r.y + f_r.z) / 3.0).clamp(0.0, 1.0);
        // Russian-roulette fresnel split: probability `f_r_avg` we
        // reflect off the microfacet, otherwise we refract.
        // Use the dedicated 4th sample for the Fresnel split so it
        // is independent of `rng[2]` (already consumed by
        // `sample_ggx_half_unconditional` as the polar-angle `u2`).
        if rng[3] < f_r_avg {
            // Fresnel reflects → behave like a specular bounce.
            let wi = 2.0 * v_dot_h * h_world - wo;
            if n.dot(wi) <= 0.0 {
                return None;
            }
            let pdf = pdf_bsdf(mat, n, wi, wo).max(1e-12);
            let f = eval_bsdf(mat, n, wi, wo);
            let cos_theta = n.dot(wi).max(0.0);
            Some(BsdfSample {
                direction: wi,
                weight: f * cos_theta / pdf,
                pdf,
                is_specular: mat.roughness < 0.05,
                is_transmission: false,
            })
        } else if let Some(wi) = refract_snell(wo, h_world, 1.0 / mat.ior) {
            // Refract through the microfacet.
            let pdf = pdf_bsdf(mat, n, wi, wo).max(1e-12);
            // For a perfectly smooth refraction the BTDF integrates
            // to `base_color * (1 - F)`; for rough glass we compute
            // the proper microfacet BTDF in eval_bsdf, but for the
            // smooth limit we use the closed-form result directly so
            // the variance stays low.
            let weight = if mat.roughness < 0.05 {
                mat.base_color * (Vec3::ONE - f_r) * mat.transmission / (1.0 - f_r_avg).max(1e-6)
            } else {
                let f = eval_bsdf(mat, n, wi, wo);
                let cos_l = n.dot(wi).abs().max(1e-6);
                f * cos_l / pdf
            };
            Some(BsdfSample {
                direction: wi,
                weight,
                pdf,
                is_specular: mat.roughness < 0.05,
                is_transmission: true,
            })
        } else {
            // Total internal reflection → reflect specularly.
            let wi = 2.0 * v_dot_h * h_world - wo;
            if n.dot(wi) <= 0.0 {
                return None;
            }
            let pdf = pdf_bsdf(mat, n, wi, wo).max(1e-12);
            let f = eval_bsdf(mat, n, wi, wo);
            let cos_theta = n.dot(wi).max(0.0);
            Some(BsdfSample {
                direction: wi,
                weight: f * cos_theta / pdf,
                pdf,
                is_specular: mat.roughness < 0.05,
                is_transmission: false,
            })
        }
    } else {
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
            is_transmission: false,
        })
    }
}

/// Sample a half-vector from the GGX distribution **without** rejecting
/// when it doesn't reflect `wo` into the upper hemisphere — needed by
/// the transmission path because the reflected direction may legitimately
/// dip below `n`.
fn sample_ggx_half_unconditional(n: Vec3, t: Vec3, b: Vec3, alpha: f32, u1: f32, u2: f32) -> Vec3 {
    let phi = 2.0 * PI * u1;
    let cos_theta = ((1.0 - u2) / (u2 * (alpha * alpha - 1.0) + 1.0))
        .sqrt()
        .clamp(0.0, 1.0);
    let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
    (t * (phi.cos() * sin_theta) + b * (phi.sin() * sin_theta) + n * cos_theta).normalize_or_zero()
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
///
/// `sign` is computed as a strict `±1.0` (never `0.0`) because Rust's
/// `f32::signum(0.0) == 0.0`, which would propagate `-inf` / `NaN` through
/// the Duff construction for normals with exactly `n.z == 0.0` (i.e. walls
/// in the `XY` plane, which are extremely common in architectural scenes).
pub fn tangent_basis(n: Vec3) -> (Vec3, Vec3) {
    let sign: f32 = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    let t = Vec3::new(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x);
    let bt = Vec3::new(b, sign + n.y * n.y * a, -n.y);
    (t.normalize(), bt.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::TextureId;

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
            texture_bindings: None,
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
            texture_bindings: None,
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
            texture_bindings: None,
            transmission: 0.0,
            ao: 1.0,
        };
        let n = Vec3::Z;
        let wo = Vec3::new(0.3, 0.0, 1.0).normalize();
        let n_samples = 4096;
        let mut sum = Vec3::ZERO;
        for _ in 0..n_samples {
            let r = [
                fastrand::f32(),
                fastrand::f32(),
                fastrand::f32(),
                fastrand::f32(),
            ];
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

    fn checker_texture() -> (TextureAtlas, TextureId) {
        let mut atlas = TextureAtlas::new();
        // 2x2 checker: black, white / white, black, stored as
        // linear-space RGBA.
        let pixels = vec![
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let id = atlas.register_linear_rgba_f32(2, 2, pixels);
        (atlas, id)
    }

    #[test]
    fn sample_textured_material_resolves_albedo() {
        let (atlas, id) = checker_texture();
        let mat = PathTraceMaterial {
            base_color: Vec3::ONE,
            texture_bindings: Some(MaterialTextureBindings {
                albedo: Some(id),
                normal: None,
                metallic_roughness: None,
                emissive: None,
            }),
            ..PathTraceMaterial::default_grey()
        };
        // NOTE: TextureAtlas flips V to match glTF convention, so the
        // top-row pixels in the source array become the bottom row in
        // (u, v) space and vice versa.
        //  source row 0 (top): black, white
        //  source row 1 (bot): white, black
        //  ↓ V-flip                          ↓
        //  uv row v=0 (bot):  white, black     ← (0.25, 0.25) → white
        //  uv row v=1 (top):  black, white     ← (0.25, 0.75) → black
        let s = sample_textured_material(&mat, &atlas, Vec2::new(0.25, 0.75));
        assert!(
            s.base_color.length_squared() < 1e-4,
            "expected ~black at (0.25, 0.75), got {:?}",
            s.base_color
        );
        let s = sample_textured_material(&mat, &atlas, Vec2::new(0.75, 0.75));
        assert!(
            s.base_color.x > 0.9 && s.base_color.y > 0.9 && s.base_color.z > 0.9,
            "expected ~white at (0.75, 0.75), got {:?}",
            s.base_color
        );
    }

    #[test]
    fn sample_textured_material_with_no_bindings_is_identity() {
        let atlas = TextureAtlas::new();
        let mat = PathTraceMaterial::default_grey();
        let s = sample_textured_material(&mat, &atlas, Vec2::new(0.5, 0.5));
        assert!(approx(s.base_color, mat.base_color, 1e-5));
    }

    #[test]
    fn refract_snell_returns_none_on_tir() {
        // At grazing incidence inside glass (eta = 1.5), no light can
        // refract out — TIR.
        let n = Vec3::Z;
        let wo = Vec3::new(0.99, 0.0, 0.05).normalize();
        let result = refract_snell(wo, n, 1.5);
        assert!(result.is_none(), "expected TIR, got {result:?}");
    }

    #[test]
    fn refract_snell_normal_incidence_passes_through() {
        let n = Vec3::Z;
        let wo = Vec3::Z;
        let r = refract_snell(wo, n, 1.0 / 1.5).expect("should refract");
        // Light entering glass at normal incidence travels straight.
        assert!(approx(r, -Vec3::Z, 1e-5), "got {r:?}");
    }

    #[test]
    fn glass_sample_picks_transmission_majority_of_time() {
        // A pure-glass material with transmission=1 should pick the
        // transmission lobe with high probability.
        let mat = PathTraceMaterial {
            base_color: Vec3::ONE,
            metallic: 0.0,
            roughness: 0.0,
            ior: 1.5,
            emissive: Vec3::ZERO,
            transmission: 1.0,
            ao: 1.0,
            texture_bindings: None,
        };
        let n = Vec3::Z;
        let wo = Vec3::Z;
        let mut count_trans = 0;
        let trials = 200;
        for i in 0..trials {
            let r = [
                (i as f32 + 0.5) / trials as f32,
                fastrand::f32(),
                fastrand::f32(),
                fastrand::f32(),
            ];
            if let Some(s) = sample_bsdf(&mat, n, wo, r) {
                if s.is_transmission {
                    count_trans += 1;
                }
            }
        }
        assert!(
            count_trans > (trials * 9) / 10,
            "expected >90% transmission samples for glass, got {count_trans}/{trials}"
        );
    }

    #[test]
    fn opaque_material_never_picks_transmission() {
        let mat = PathTraceMaterial::default_grey();
        let n = Vec3::Z;
        let wo = Vec3::Z;
        for i in 0..50 {
            let r = [
                fastrand::f32(),
                (i as f32 + 0.5) / 50.0,
                fastrand::f32(),
                fastrand::f32(),
            ];
            if let Some(s) = sample_bsdf(&mat, n, wo, r) {
                assert!(!s.is_transmission, "opaque material picked transmission");
            }
        }
    }

    #[test]
    fn metal_never_picks_transmission_even_with_transmission_flag() {
        // Metals never transmit regardless of the transmission slider.
        let mat = PathTraceMaterial {
            base_color: Vec3::splat(0.8),
            metallic: 1.0,
            roughness: 0.2,
            ior: 1.5,
            emissive: Vec3::ZERO,
            transmission: 1.0, // ignored because metallic = 1
            ao: 1.0,
            texture_bindings: None,
        };
        assert_eq!(transmission_weight(&mat), 0.0);
    }
}
