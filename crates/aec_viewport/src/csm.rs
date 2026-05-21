//! Cascaded shadow maps (CSM) for the navigable viewport.
//!
//! Splits the view frustum into N cascades (default 3), each producing
//! its own orthographic light-space projection sized tightly to that
//! slice. Fragments then sample the cascade whose split range contains
//! their view-space depth.
//!
//! This module is pure math — frustum splits, world-space corner
//! reconstruction, tight ortho proj per cascade — so the GPU side can
//! upload N matrices and the WGSL shader can read them with no math
//! beyond `dot(p, light_view_proj)`.
//!
//! References:
//! - Practical Split Scheme (PSSM), Zhang 2007.
//! - GPU Gems 3 chapter 10 (Dimitrov, 2007).
//!
//! Optional PCSS isn't implemented in math here (it's a sampling
//! technique in the fragment shader); the [`CsmParams::soft_shadows`]
//! flag is forwarded to the shader uniform and the shader chooses the
//! filter kernel.

use glam::{Mat4, Vec3, Vec4};
use serde::{Deserialize, Serialize};

/// Maximum number of cascades supported. Hard-coded to match the
/// fixed-size uniform array on the GPU side. 4 is the common production
/// upper bound; the default is 3.
pub const MAX_CASCADES: usize = 4;

/// A single split boundary in normalised view-frustum z, `[0, 1]`. The
/// first split is implicitly `0.0` (the near plane); each cascade
/// covers `[prev_split, this_split]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CsmSplit(pub f32);

impl CsmSplit {
    pub fn clamped(self) -> f32 {
        self.0.clamp(0.0, 1.0)
    }
}

/// Parameters controlling cascade construction. Defaults are tuned for
/// architectural scenes (lots of mid-range detail, few near-camera
/// surfaces) — the practical scheme gives more resolution to the
/// near cascade than a pure logarithmic split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsmParams {
    /// Normalised split distances, **strictly increasing**, in
    /// `(0.0, 1.0)`. The number of cascades is `splits.len() + 1`.
    /// Default: `[0.05, 0.20, 0.55]` (4 cascades).
    pub splits: Vec<CsmSplit>,
    /// Shadow map resolution per cascade. Default `2048`.
    pub resolution: u32,
    /// World-space padding around each cascade frustum AABB. Helps
    /// reduce edge bleed under camera rotation. Default `0.5`.
    pub padding: f32,
    /// When `true`, the shader uses a contact-hardening (PCSS) kernel
    /// instead of fixed-radius PCF. Off by default — PCF is faster and
    /// looks correct for the AEC workload.
    pub soft_shadows: bool,
    /// Number of stabilisation snaps per pixel: rounds the cascade
    /// center to a texel grid so the shadow doesn't shimmer when the
    /// camera moves slowly. Set to `0` to disable. Default `1`.
    pub stabilise_to_texel: u32,
}

impl Default for CsmParams {
    fn default() -> Self {
        Self {
            splits: vec![CsmSplit(0.05), CsmSplit(0.20), CsmSplit(0.55)],
            resolution: 2048,
            padding: 0.5,
            soft_shadows: false,
            stabilise_to_texel: 1,
        }
    }
}

impl CsmParams {
    pub fn cascade_count(&self) -> usize {
        (self.splits.len() + 1).min(MAX_CASCADES)
    }

    /// Resolve the absolute split distances for a given near/far. The
    /// returned slice has length `cascade_count() + 1` and starts with
    /// `near` and ends with `far`.
    pub fn absolute_splits(&self, near: f32, far: f32) -> Vec<f32> {
        let cnt = self.cascade_count();
        let mut out = Vec::with_capacity(cnt + 1);
        out.push(near);
        for s in &self.splits {
            out.push(near + s.clamped() * (far - near));
        }
        out.truncate(cnt);
        out.push(far);
        out
    }
}

/// Result of building a single cascade: a tight light-space ortho
/// projection plus the metadata the shader uses to decide which
/// cascade applies to a given fragment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CascadeSlice {
    /// View-space depth where this cascade starts. The shader picks
    /// the smallest-index cascade whose `[near_view, far_view]`
    /// contains the fragment's `view_z`.
    pub near_view: f32,
    pub far_view: f32,
    /// Light-space `clip-from-world` matrix for the cascade. The
    /// shader transforms `world_pos` by this matrix to sample the
    /// depth map.
    pub light_view_proj: Mat4,
    /// World-space center of the cascade's frustum slice. Used as the
    /// look-at target for the light view (and exposed in case the
    /// caller wants to debug-visualise it).
    pub world_center: Vec3,
    /// World-space radius of the cascade's bounding sphere. Used as
    /// the ortho proj's half-extent.
    pub world_radius: f32,
}

/// Build N [`CascadeSlice`]s for the given camera + sun.
///
/// `inv_view_proj` is the inverse of the camera's `clip-from-world`
/// matrix (in wgpu / D3D z-`[0,1]` clip space). `near`/`far` are the
/// camera's near/far planes. `sun_direction` is the unit vector
/// pointing **from ground to sun** (matches [`crate::pbr_preview::SunLight::direction`]).
pub fn build_cascades(
    params: &CsmParams,
    inv_view_proj: Mat4,
    near: f32,
    far: f32,
    sun_direction: Vec3,
) -> Vec<CascadeSlice> {
    let splits = params.absolute_splits(near, far);
    let cnt = params.cascade_count();
    let mut out = Vec::with_capacity(cnt);
    for i in 0..cnt {
        let s_near = splits[i];
        let s_far = splits[i + 1];
        let slice = build_one(
            params,
            inv_view_proj,
            s_near,
            s_far,
            near,
            far,
            sun_direction,
        );
        out.push(slice);
    }
    out
}

fn build_one(
    params: &CsmParams,
    inv_view_proj: Mat4,
    s_near: f32,
    s_far: f32,
    cam_near: f32,
    cam_far: f32,
    sun_direction: Vec3,
) -> CascadeSlice {
    // Normalised z within the camera's near/far. Same z used to pick
    // the cascade in the shader.
    let denom = (cam_far - cam_near).max(1e-6);
    let z_near_n = ((s_near - cam_near) / denom).clamp(0.0, 1.0);
    let z_far_n = ((s_far - cam_near) / denom).clamp(0.0, 1.0);

    // Reconstruct the 8 world-space corners of this slice. Using NDC
    // z = [0,1] for wgpu / D3D clip space.
    let mut corners: [Vec3; 8] = [Vec3::ZERO; 8];
    let mut idx = 0;
    for z_n in [z_near_n, z_far_n] {
        // wgpu clip: z in [0,1] => z_ndc = z_n directly (linear in clip space).
        for y in [-1.0_f32, 1.0_f32] {
            for x in [-1.0_f32, 1.0_f32] {
                let clip = Vec4::new(x, y, z_n, 1.0);
                let world = inv_view_proj * clip;
                if world.w.abs() > 1e-8 {
                    corners[idx] =
                        Vec3::new(world.x / world.w, world.y / world.w, world.z / world.w);
                } else {
                    corners[idx] = Vec3::ZERO;
                }
                idx += 1;
            }
        }
    }

    // Bounding sphere of the slice (more stable under camera rotation
    // than an AABB). Use the centroid + max distance to any corner.
    let mut center = Vec3::ZERO;
    for c in &corners {
        center += *c;
    }
    center /= corners.len() as f32;
    let mut max_r = 0.0_f32;
    for c in &corners {
        let d = (*c - center).length();
        if d > max_r {
            max_r = d;
        }
    }
    let radius = max_r + params.padding.max(0.0);

    // Stabilise the center to a texel grid so the shadow doesn't
    // shimmer when the camera moves slowly. The texel size in world
    // units is `2*radius / resolution`.
    let stabilised_center = if params.stabilise_to_texel > 0 {
        let texel_size = (radius * 2.0) / params.resolution.max(1) as f32;
        let snap = texel_size * params.stabilise_to_texel as f32;
        Vec3::new(
            (center.x / snap).round() * snap,
            (center.y / snap).round() * snap,
            (center.z / snap).round() * snap,
        )
    } else {
        center
    };

    // Build the light view: eye = center + dir * 2*radius (beyond the
    // sphere so the near plane stays positive). Up vector picked to
    // avoid collinearity with a near-vertical sun.
    let dir = sun_direction.normalize_or_zero();
    let eye = stabilised_center + dir * (radius.max(1.0) * 2.0);
    let up = if dir.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(eye, stabilised_center, up);
    let half = radius.max(1.0);
    let proj = Mat4::orthographic_rh(-half, half, -half, half, 0.1, half * 4.0 + 0.1);

    CascadeSlice {
        near_view: s_near,
        far_view: s_far,
        light_view_proj: proj * view,
        world_center: stabilised_center,
        world_radius: radius,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_view_proj() -> Mat4 {
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 16.0 / 9.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO, Vec3::Y);
        proj * view
    }

    #[test]
    fn default_params_have_three_splits_and_four_cascades() {
        let p = CsmParams::default();
        assert_eq!(p.splits.len(), 3);
        assert_eq!(p.cascade_count(), 4);
    }

    #[test]
    fn absolute_splits_include_near_and_far() {
        let p = CsmParams::default();
        let absolute = p.absolute_splits(0.5, 200.0);
        assert_eq!(absolute.len(), p.cascade_count() + 1);
        assert!((absolute[0] - 0.5).abs() < 1e-5);
        assert!((absolute.last().copied().unwrap() - 200.0).abs() < 1e-5);
        // Monotonic.
        for w in absolute.windows(2) {
            assert!(w[1] > w[0], "expected strictly increasing absolute splits");
        }
    }

    #[test]
    fn cascade_count_clamps_to_max() {
        let p = CsmParams {
            splits: vec![CsmSplit(0.05); MAX_CASCADES + 4],
            ..Default::default()
        };
        assert_eq!(p.cascade_count(), MAX_CASCADES);
    }

    #[test]
    fn build_cascades_returns_expected_count() {
        let p = CsmParams::default();
        let vp = camera_view_proj();
        let inv = vp.inverse();
        let cascades = build_cascades(&p, inv, 0.1, 100.0, Vec3::new(0.3, 0.9, 0.3));
        assert_eq!(cascades.len(), p.cascade_count());
    }

    #[test]
    fn cascade_near_far_match_absolute_splits() {
        let p = CsmParams::default();
        let vp = camera_view_proj();
        let inv = vp.inverse();
        let cascades = build_cascades(&p, inv, 0.1, 100.0, Vec3::new(0.0, 1.0, 0.0));
        let absolute = p.absolute_splits(0.1, 100.0);
        for (i, c) in cascades.iter().enumerate() {
            assert!((c.near_view - absolute[i]).abs() < 1e-4);
            assert!((c.far_view - absolute[i + 1]).abs() < 1e-4);
        }
    }

    #[test]
    fn cascade_radii_grow_with_distance() {
        let p = CsmParams::default();
        let vp = camera_view_proj();
        let inv = vp.inverse();
        let cascades = build_cascades(&p, inv, 0.1, 100.0, Vec3::new(0.0, 1.0, 0.0));
        for w in cascades.windows(2) {
            assert!(
                w[1].world_radius >= w[0].world_radius,
                "expected far cascade to be no smaller than near cascade"
            );
        }
    }

    #[test]
    fn stabilisation_snaps_center_to_grid() {
        let p = CsmParams {
            stabilise_to_texel: 1,
            resolution: 1024,
            ..Default::default()
        };
        let p_off = CsmParams {
            stabilise_to_texel: 0,
            ..p.clone()
        };
        let vp = camera_view_proj();
        let inv = vp.inverse();
        let on = build_cascades(&p, inv, 0.1, 100.0, Vec3::new(0.3, 0.9, 0.3));
        let off = build_cascades(&p_off, inv, 0.1, 100.0, Vec3::new(0.3, 0.9, 0.3));
        // At least one cascade should have moved its center under stabilisation.
        let moved = on
            .iter()
            .zip(off.iter())
            .any(|(a, b)| (a.world_center - b.world_center).length() > 1e-6);
        assert!(moved);
    }

    #[test]
    fn near_vertical_sun_uses_z_up_to_avoid_collinearity() {
        let p = CsmParams::default();
        let vp = camera_view_proj();
        let inv = vp.inverse();
        let cascades = build_cascades(&p, inv, 0.1, 100.0, Vec3::new(0.0, 1.0, 0.0));
        // Just check the matrix is finite (collinear up would produce NaNs).
        for c in cascades {
            for col in c.light_view_proj.to_cols_array() {
                assert!(col.is_finite(), "expected finite matrix entries");
            }
        }
    }
}
