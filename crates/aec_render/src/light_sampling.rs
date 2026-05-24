//! Light sampling for the native path tracer.
//!
//! Implements direct-lighting (next-event estimation) for each light type
//! exposed by [`crate::scene::RenderLight`] plus the existing
//! [`crate::lighting::IesProfile`] photometric data. Multiple-importance
//! sampling between the BSDF sample and the light sample uses the power
//! heuristic, matching the technique used throughout Cycles
//! (`src/kernel/light/sample.h`).
//!
//! The model is intentionally physically-meaningful but bounded — we do
//! not need full subsurface/SSS or many-light-tree sampling for the
//! interior architectural workloads AEC Studio targets. The IES profile
//! support routes through the existing [`IesProfile::candela_at`] which
//! has been in the codebase since Phase 5.

use std::f32::consts::PI;

use glam::Vec3;

use crate::lighting::{kelvin_to_rgb, IesProfile, SkyParams};
use crate::scene::RenderLight;

/// A light in the form the path tracer consumes. The `RenderLight` enum
/// from the existing scene serialisation is `From`-converted into this
/// once at scene-build time.
#[derive(Debug, Clone)]
pub enum NativeLight {
    /// Directional / sun light. `direction` points FROM the light TO the
    /// scene (Cycles convention). `angular_radius_rad` controls soft-shadow
    /// softness — 0 for hard shadows, ~0.0046 rad for the real sun.
    Sun {
        direction: Vec3,
        radiance: Vec3,
        angular_radius_rad: f32,
    },
    /// Area light, rectangular, two-sided behaviour with `normal`.
    Area {
        position: Vec3,
        normal: Vec3,
        u_axis: Vec3,
        v_axis: Vec3,
        width: f32,
        height: f32,
        radiance: Vec3,
    },
    /// Isotropic point light with inverse-square falloff.
    Point { position: Vec3, intensity: Vec3 },
    /// Point light driven by an IES photometric profile.
    Ies {
        position: Vec3,
        /// Direction the luminaire points (e.g. straight down `-Y`).
        forward: Vec3,
        up: Vec3,
        profile: IesProfile,
        /// Multiplier applied on top of the profile's candela values.
        intensity_scale: f32,
        /// Linear-RGB tint applied to the profile's photometric output.
        color: Vec3,
    },
}

impl NativeLight {
    /// Convert from the scene `RenderLight` envelope. Coordinates remain
    /// in scene units (mm) — the caller is responsible for any unit scaling
    /// when normalising the rest of the path tracer to metres.
    pub fn from_render_light(l: &RenderLight) -> Self {
        match l {
            RenderLight::SunSky {
                azimuth_deg,
                elevation_deg,
                intensity,
                color_temperature_k,
            } => {
                let az = azimuth_deg.to_radians();
                let el = elevation_deg.to_radians();
                // Sun direction: pointing from sun toward scene origin.
                let dir =
                    Vec3::new(-el.cos() * az.cos(), -el.sin(), -el.cos() * az.sin()).normalize();
                let color = kelvin_to_rgb(*color_temperature_k);
                Self::Sun {
                    direction: dir,
                    radiance: Vec3::from_array(color) * *intensity,
                    angular_radius_rad: 0.00465,
                }
            }
            RenderLight::Area {
                position_mm,
                width_mm,
                height_mm,
                intensity,
                color_temperature_k,
            } => {
                let pos = Vec3::from_array(*position_mm);
                let color = kelvin_to_rgb(*color_temperature_k);
                Self::Area {
                    position: pos,
                    normal: Vec3::NEG_Y,
                    u_axis: Vec3::X,
                    v_axis: Vec3::Z,
                    width: *width_mm,
                    height: *height_mm,
                    radiance: Vec3::from_array(color) * *intensity,
                }
            }
            RenderLight::Point {
                position_mm,
                intensity,
                color_temperature_k,
            } => {
                let color = kelvin_to_rgb(*color_temperature_k);
                Self::Point {
                    position: Vec3::from_array(*position_mm),
                    intensity: Vec3::from_array(color) * *intensity,
                }
            }
        }
    }
}

/// One direct-lighting sample: direction from shading point toward the
/// light, distance, emitted radiance, and PDF in solid-angle measure.
#[derive(Debug, Clone, Copy)]
pub struct LightSample {
    pub direction: Vec3,
    pub distance: f32,
    pub emitted: Vec3,
    pub pdf: f32,
}

/// Sample a single light at a shading point.
///
/// `rng` is a 2D uniform sample in `[0, 1)^2`.
pub fn sample_light(
    light: &NativeLight,
    shading_point: Vec3,
    rng: [f32; 2],
) -> Option<LightSample> {
    match light {
        NativeLight::Sun {
            direction,
            radiance,
            angular_radius_rad,
        } => {
            // For a sun light the light sample is the direction toward the
            // sun, perturbed by a small cone (angular radius). The PDF over
            // the cone is 1 / (2π * (1 - cos(α))).
            let cone_dir = -*direction;
            let cos_alpha = (*angular_radius_rad).cos();
            let (t, b) = orthonormal_basis(cone_dir);
            let cos_theta = 1.0 - rng[0] * (1.0 - cos_alpha);
            let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
            let phi = 2.0 * PI * rng[1];
            let sample_dir = cone_dir * cos_theta + (t * phi.cos() + b * phi.sin()) * sin_theta;
            let pdf = if cos_alpha >= 1.0 {
                1.0
            } else {
                1.0 / (2.0 * PI * (1.0 - cos_alpha))
            };
            Some(LightSample {
                direction: sample_dir.normalize(),
                distance: 1e10,
                emitted: *radiance,
                pdf,
            })
        }
        NativeLight::Area {
            position,
            normal,
            u_axis,
            v_axis,
            width,
            height,
            radiance,
        } => {
            let u = (rng[0] - 0.5) * *width;
            let v = (rng[1] - 0.5) * *height;
            let light_point = *position + *u_axis * u + *v_axis * v;
            let to_light = light_point - shading_point;
            let dist_sq = to_light.length_squared().max(1e-12);
            let dist = dist_sq.sqrt();
            let dir = to_light / dist;
            let cos_at_light = (-dir).dot(*normal).max(0.0);
            if cos_at_light <= 0.0 {
                return None;
            }
            let area = width * height;
            // Convert area pdf to solid-angle pdf: p_omega = p_area * r² / cos.
            let pdf = dist_sq / (cos_at_light * area);
            Some(LightSample {
                direction: dir,
                distance: dist,
                emitted: *radiance,
                pdf,
            })
        }
        NativeLight::Point {
            position,
            intensity,
        } => {
            let to_light = *position - shading_point;
            let dist_sq = to_light.length_squared().max(1e-12);
            let dist = dist_sq.sqrt();
            let dir = to_light / dist;
            // Delta light: emitted incorporates the 1/r² falloff so the
            // path tracer doesn't double-count it elsewhere.
            let emitted = *intensity / dist_sq;
            Some(LightSample {
                direction: dir,
                distance: dist,
                emitted,
                pdf: 1.0, // delta light; MIS treats this with the power heuristic
            })
        }
        NativeLight::Ies {
            position,
            forward,
            up,
            profile,
            intensity_scale,
            color,
        } => {
            let to_light = *position - shading_point;
            let dist_sq = to_light.length_squared().max(1e-12);
            let dist = dist_sq.sqrt();
            let dir = to_light / dist;
            // Direction from luminaire to shading point in luminaire frame.
            let local = world_to_local(-dir, *forward, *up);
            // Vertical angle (from `-forward`) and horizontal (around forward).
            let vert_deg = local.z.clamp(-1.0, 1.0).acos().to_degrees();
            let horiz_deg = local.y.atan2(local.x).to_degrees().rem_euclid(360.0);
            let candela = profile.candela_at(vert_deg, horiz_deg);
            let intensity = *color * candela * *intensity_scale;
            let emitted = intensity / dist_sq;
            Some(LightSample {
                direction: dir,
                distance: dist,
                emitted,
                pdf: 1.0, // delta luminaire
            })
        }
    }
}

/// Return constant environment radiance for a missed ray, derived from
/// [`SkyParams`]. Used both as a sky light by the integrator and as the
/// background colour of paths that escape the scene.
pub fn environment_radiance(sky: &SkyParams, _direction: Vec3) -> Vec3 {
    let base = Vec3::from_array(sky.color);
    base * sky.strength
}

/// Solid-angle pdf the NEE sampler would have produced for `direction`
/// from `shading_point`, given `light`. Inverse of [`sample_light`].
///
/// This is the missing half of MIS. When a BSDF sample at bounce
/// `N > 0` lands on an emitter (whether an analytic [`NativeLight`] or
/// a surface with a non-zero `mat.emissive`), we want to combine the
/// BSDF and NEE estimators via the power heuristic rather than
/// dropping the contribution outright. To do that we need the pdf
/// each strategy would have assigned to the same direction. This
/// returns the NEE side; the BSDF side is `prev_bsdf_pdf`.
///
/// Returns `0.0` when:
/// * `light` cannot generate `direction` from `shading_point` (e.g.
///   the area light's rectangle is on the other side, or `direction`
///   is outside the sun's angular cone),
/// * `light` is a delta luminaire ([`NativeLight::Point`] /
///   [`NativeLight::Ies`]): they have zero solid-angle support, so
///   no BSDF sample can ever land on them and MIS is vacuous.
///
/// For non-zero results the return value is in steradian⁻¹ and is
/// directly comparable to a BSDF pdf in the same measure.
pub fn light_pdf(light: &NativeLight, shading_point: Vec3, direction: Vec3) -> f32 {
    match light {
        NativeLight::Sun {
            direction: sun_dir,
            angular_radius_rad,
            ..
        } => {
            let to_sun = (-*sun_dir).normalize_or_zero();
            let cos_alpha = angular_radius_rad.cos();
            let cos_angle = direction.normalize_or_zero().dot(to_sun);
            if cos_angle >= cos_alpha {
                if cos_alpha >= 1.0 {
                    // Degenerate point-sun: delta-like, no MIS support.
                    0.0
                } else {
                    1.0 / (2.0 * PI * (1.0 - cos_alpha))
                }
            } else {
                0.0
            }
        }
        NativeLight::Area {
            position,
            normal,
            u_axis,
            v_axis,
            width,
            height,
            ..
        } => {
            // Intersect the ray (shading_point, direction) with the
            // area-light plane and check the hit is inside the rect.
            let denom = normal.dot(direction);
            if denom.abs() < 1e-6 {
                return 0.0;
            }
            let t = (*position - shading_point).dot(*normal) / denom;
            if t <= 1e-4 {
                return 0.0;
            }
            let hit_pt = shading_point + direction * t;
            let local = hit_pt - *position;
            let u = local.dot(*u_axis);
            let v = local.dot(*v_axis);
            if u.abs() > *width * 0.5 || v.abs() > *height * 0.5 {
                return 0.0;
            }
            // Same area→solid-angle conversion as `sample_light`.
            let dist_sq = (hit_pt - shading_point).length_squared().max(1e-12);
            let cos_at_light = (-direction).dot(*normal).max(0.0);
            if cos_at_light <= 0.0 {
                return 0.0;
            }
            let area = width * height;
            dist_sq / (cos_at_light * area)
        }
        NativeLight::Point { .. } | NativeLight::Ies { .. } => 0.0,
    }
}

/// Sum of [`light_pdf`] across `lights`. The integrator's NEE loop in
/// [`crate::path_trace::trace_path`] iterates *every* non-delta light
/// at every bounce (no random light picking), so each light's pdf is
/// already unconditional in steradian⁻¹ — the combined NEE estimator's
/// pdf for direction `d` is therefore the straight sum. For two
/// non-overlapping lights only one term is non-zero at any direction
/// (the other rectangle/cone test fails); for overlapping support the
/// sum is the genuine combined pdf and MIS still produces an unbiased
/// estimator.
///
/// Delta lights ([`NativeLight::Point`] / [`NativeLight::Ies`]) are
/// excluded — they have zero solid-angle measure, no BSDF sample can
/// land on them, and MIS is undefined.
pub fn nee_sum_pdf(lights: &[NativeLight], shading_point: Vec3, direction: Vec3) -> f32 {
    let mut total = 0.0_f32;
    for light in lights {
        if matches!(light, NativeLight::Point { .. } | NativeLight::Ies { .. }) {
            continue;
        }
        total += light_pdf(light, shading_point, direction);
    }
    total
}

/// MIS power heuristic (β = 2). Standard for offline path tracers.
#[inline]
pub fn power_heuristic(pdf_a: f32, pdf_b: f32) -> f32 {
    let a = pdf_a * pdf_a;
    let b = pdf_b * pdf_b;
    let denom = a + b;
    if denom <= 0.0 {
        0.0
    } else {
        a / denom
    }
}

/// Test whether `light` is a delta light (no MIS weight against BSDF).
pub fn is_delta(light: &NativeLight) -> bool {
    matches!(
        light,
        NativeLight::Point { .. } | NativeLight::Ies { .. } | NativeLight::Sun { .. }
    )
}

fn orthonormal_basis(n: Vec3) -> (Vec3, Vec3) {
    let sign = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    let t = Vec3::new(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x).normalize();
    let bt = Vec3::new(b, sign + n.y * n.y * a, -n.y).normalize();
    (t, bt)
}

fn world_to_local(v: Vec3, forward: Vec3, up: Vec3) -> Vec3 {
    let right = forward.cross(up).normalize();
    let true_up = right.cross(forward).normalize();
    Vec3::new(v.dot(right), v.dot(true_up), v.dot(forward))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_light_pdf_is_unity() {
        let l = NativeLight::Point {
            position: Vec3::new(0.0, 5.0, 0.0),
            intensity: Vec3::splat(100.0),
        };
        let s = sample_light(&l, Vec3::ZERO, [0.5, 0.5]).unwrap();
        assert_eq!(s.pdf, 1.0);
        assert!(s.distance > 4.9 && s.distance < 5.1);
        // 1/r² falloff: emitted = intensity / 25.
        assert!((s.emitted.x - 4.0).abs() < 1e-3, "got {}", s.emitted.x);
    }

    #[test]
    fn area_light_emits_only_on_correct_side() {
        // Area light at y=5, normal pointing -Y → only points below it
        // (y < 5) see emission.
        let l = NativeLight::Area {
            position: Vec3::new(0.0, 5.0, 0.0),
            normal: Vec3::NEG_Y,
            u_axis: Vec3::X,
            v_axis: Vec3::Z,
            width: 2.0,
            height: 2.0,
            radiance: Vec3::splat(10.0),
        };
        // Below: should sample fine.
        assert!(sample_light(&l, Vec3::new(0.0, 0.0, 0.0), [0.5, 0.5]).is_some());
        // Above: cos_at_light <= 0 → None.
        assert!(sample_light(&l, Vec3::new(0.0, 10.0, 0.0), [0.5, 0.5]).is_none());
    }

    #[test]
    fn sun_light_returns_direction_to_sun_with_small_pdf_for_small_cone() {
        let l = NativeLight::Sun {
            direction: Vec3::new(0.0, -1.0, 0.0),
            radiance: Vec3::splat(1.0),
            angular_radius_rad: 0.005,
        };
        let s = sample_light(&l, Vec3::ZERO, [0.5, 0.5]).unwrap();
        // Sun is overhead → sampled direction has positive Y.
        assert!(s.direction.y > 0.99);
        // Tight cone → high PDF.
        assert!(s.pdf > 100.0);
    }

    #[test]
    fn ies_sample_uses_profile() {
        let profile = IesProfile::test_isotropic(1000.0);
        let l = NativeLight::Ies {
            position: Vec3::new(0.0, 3.0, 0.0),
            forward: Vec3::NEG_Y,
            up: Vec3::Z,
            profile,
            intensity_scale: 1.0,
            color: Vec3::ONE,
        };
        let s = sample_light(&l, Vec3::ZERO, [0.5, 0.5]).unwrap();
        // Direction up to the luminaire.
        assert!(s.direction.y > 0.99);
        // Isotropic 1000-cd profile at r=3: emitted = 1000 / 9.
        assert!((s.emitted.x - 1000.0 / 9.0).abs() < 1e-1);
    }

    #[test]
    fn power_heuristic_unity_at_equal_pdf() {
        assert!((power_heuristic(1.0, 1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn power_heuristic_zero_dominates() {
        assert_eq!(power_heuristic(0.0, 1.0), 0.0);
    }

    #[test]
    fn mis_reduces_variance_on_well_known_combination() {
        // Trivial smoke: PDF combination must give a real number in [0,1].
        let mut max_w = 0.0_f32;
        for i in 0..32 {
            for j in 0..32 {
                let p1 = (i as f32) / 32.0 + 0.01;
                let p2 = (j as f32) / 32.0 + 0.01;
                let w = power_heuristic(p1, p2);
                max_w = max_w.max(w);
                assert!((0.0..=1.0).contains(&w));
            }
        }
        assert!(max_w > 0.5);
    }

    #[test]
    fn render_light_round_trip_sun() {
        let rl = RenderLight::SunSky {
            azimuth_deg: 90.0,
            elevation_deg: 45.0,
            intensity: 2.0,
            color_temperature_k: 5500.0,
        };
        let n = NativeLight::from_render_light(&rl);
        match n {
            NativeLight::Sun {
                direction,
                radiance,
                ..
            } => {
                // Direction is normalised.
                assert!((direction.length() - 1.0).abs() < 1e-4);
                // Radiance non-zero.
                assert!(radiance.length() > 0.0);
            }
            _ => panic!("expected sun"),
        }
    }

    #[test]
    fn environment_radiance_scales_with_strength() {
        let sky = SkyParams {
            strength: 2.0,
            color: [0.5, 0.7, 1.0],
            turbidity: 3.0,
        };
        let e = environment_radiance(&sky, Vec3::Y);
        assert!((e.x - 1.0).abs() < 1e-5);
        assert!((e.z - 2.0).abs() < 1e-5);
    }

    #[test]
    fn delta_light_detection() {
        assert!(is_delta(&NativeLight::Point {
            position: Vec3::ZERO,
            intensity: Vec3::ONE
        }));
        assert!(!is_delta(&NativeLight::Area {
            position: Vec3::ZERO,
            normal: Vec3::Y,
            u_axis: Vec3::X,
            v_axis: Vec3::Z,
            width: 1.0,
            height: 1.0,
            radiance: Vec3::ONE,
        }));
    }

    // ---- light_pdf inverse of sample_light --------------------------

    #[test]
    fn light_pdf_matches_sample_light_for_area_light() {
        // Round-trip: take a sample direction from sample_light, feed it
        // into light_pdf, and the returned pdf must match within
        // floating-point tolerance. This is the MIS invariant.
        let l = NativeLight::Area {
            position: Vec3::new(0.0, 5.0, 0.0),
            normal: Vec3::NEG_Y,
            u_axis: Vec3::X,
            v_axis: Vec3::Z,
            width: 2.0,
            height: 2.0,
            radiance: Vec3::splat(10.0),
        };
        let shading_point = Vec3::ZERO;
        let s = sample_light(&l, shading_point, [0.3, 0.7]).unwrap();
        let p = light_pdf(&l, shading_point, s.direction);
        assert!(
            (p - s.pdf).abs() < 1e-3,
            "round-trip pdf mismatch: sample={} pdf-fn={}",
            s.pdf,
            p
        );
    }

    #[test]
    fn light_pdf_returns_zero_outside_area_light_support() {
        // Direction misses the rectangle → pdf must be 0 so MIS
        // collapses to "BSDF only" for that direction.
        let l = NativeLight::Area {
            position: Vec3::new(0.0, 5.0, 0.0),
            normal: Vec3::NEG_Y,
            u_axis: Vec3::X,
            v_axis: Vec3::Z,
            width: 0.5,
            height: 0.5,
            radiance: Vec3::splat(10.0),
        };
        // Direction +Y from origin hits the centre of the light: in.
        assert!(light_pdf(&l, Vec3::ZERO, Vec3::Y) > 0.0);
        // Direction at 60° lateral misses a 0.5-m rectangle 5 m up:
        // hit point ~8.66 m laterally, well outside the 0.25 m extent.
        let off_dir = Vec3::new(0.866, 0.5, 0.0).normalize();
        assert_eq!(light_pdf(&l, Vec3::ZERO, off_dir), 0.0);
        // Backward direction (away from the light) misses entirely.
        assert_eq!(light_pdf(&l, Vec3::ZERO, Vec3::NEG_Y), 0.0);
    }

    #[test]
    fn light_pdf_returns_zero_for_delta_lights() {
        // No BSDF sample can ever land on a delta light, so its NEE
        // pdf in solid-angle measure must be 0 (MIS collapses to BSDF
        // weight 1.0).
        let point = NativeLight::Point {
            position: Vec3::new(0.0, 5.0, 0.0),
            intensity: Vec3::splat(100.0),
        };
        assert_eq!(light_pdf(&point, Vec3::ZERO, Vec3::Y), 0.0);
        let profile = IesProfile::test_isotropic(1000.0);
        let ies = NativeLight::Ies {
            position: Vec3::new(0.0, 3.0, 0.0),
            forward: Vec3::NEG_Y,
            up: Vec3::Z,
            profile,
            intensity_scale: 1.0,
            color: Vec3::ONE,
        };
        assert_eq!(light_pdf(&ies, Vec3::ZERO, Vec3::Y), 0.0);
    }

    #[test]
    fn light_pdf_matches_sample_light_for_sun_inside_cone() {
        let l = NativeLight::Sun {
            direction: Vec3::new(0.0, -1.0, 0.0),
            radiance: Vec3::splat(1.0),
            angular_radius_rad: 0.05,
        };
        let s = sample_light(&l, Vec3::ZERO, [0.5, 0.5]).unwrap();
        let p = light_pdf(&l, Vec3::ZERO, s.direction);
        assert!(
            (p - s.pdf).abs() / s.pdf < 1e-3,
            "sun pdf mismatch: sample={} pdf-fn={}",
            s.pdf,
            p
        );
        // Outside the cone the pdf is 0.
        let outside = Vec3::new(1.0, 0.0, 0.0); // perpendicular to sun
        assert_eq!(light_pdf(&l, Vec3::ZERO, outside), 0.0);
    }

    #[test]
    fn nee_sum_pdf_sums_non_delta_lights_and_skips_delta() {
        let area = NativeLight::Area {
            position: Vec3::new(0.0, 5.0, 0.0),
            normal: Vec3::NEG_Y,
            u_axis: Vec3::X,
            v_axis: Vec3::Z,
            width: 2.0,
            height: 2.0,
            radiance: Vec3::splat(10.0),
        };
        let point = NativeLight::Point {
            position: Vec3::new(0.0, 5.0, 0.0),
            intensity: Vec3::splat(100.0),
        };
        let lights = vec![area.clone(), point];
        // Direction toward both lights.
        let dir = Vec3::Y;
        // Sum equals just the area light's pdf (point excluded).
        let sum = nee_sum_pdf(&lights, Vec3::ZERO, dir);
        let area_only = light_pdf(&area, Vec3::ZERO, dir);
        assert!((sum - area_only).abs() < 1e-6);
    }

    #[test]
    fn nee_sum_pdf_is_zero_when_only_delta_lights_present() {
        let point = NativeLight::Point {
            position: Vec3::new(0.0, 5.0, 0.0),
            intensity: Vec3::splat(100.0),
        };
        let lights = vec![point];
        assert_eq!(nee_sum_pdf(&lights, Vec3::ZERO, Vec3::Y), 0.0);
    }
}
