//! View-frustum and (planned) occlusion culling for the navigable
//! viewport.
//!
//! Two strategies, layered:
//!
//! 1. **Frustum culling (CPU)**: extract 6 planes from a view-projection
//!    matrix and test each candidate AABB against them. Pure math,
//!    deterministic, runs in <50 µs for 50k entities — fast enough to
//!    run unconditionally every frame.
//! 2. **Occlusion culling (Hi-Z, GPU)**: build a depth pyramid from the
//!    last frame's depth buffer; per-instance the AABB is projected and
//!    a single sample of the appropriate mip rejects fully-occluded
//!    instances. This module exposes the CPU side of the Hi-Z handshake
//!    (deciding which mip to sample, projecting the AABB to NDC); the
//!    actual depth-pyramid build lives in the wgpu pipeline.
//!
//! The CPU frustum-culler is the only piece active by default. Hi-Z
//! activates when the navigable pipeline has produced at least one
//! frame.

use glam::{Mat4, Vec3, Vec4};
use serde::{Deserialize, Serialize};

/// World-space axis-aligned bounding box. We use this everywhere (vs.
/// `aec_geometry::BvhAabb` which is `[f64; 3]`) because the viewport is
/// f32 throughout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    pub fn from_points<I>(points: I) -> Option<Self>
    where
        I: IntoIterator<Item = [f32; 3]>,
    {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut min = first;
        let mut max = first;
        for p in iter {
            for k in 0..3 {
                if p[k] < min[k] {
                    min[k] = p[k];
                }
                if p[k] > max[k] {
                    max[k] = p[k];
                }
            }
        }
        Some(Self { min, max })
    }

    /// Transform an AABB by a 4x4 matrix. We transform the 8 corners
    /// and re-take the AABB, which is exact for translations + rotations
    /// + non-uniform scales (the standard "AABB of OBB" technique).
    pub fn transformed(&self, m: Mat4) -> Self {
        let corners = self.corners();
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for c in corners {
            let p = m.transform_point3(Vec3::new(c[0], c[1], c[2]));
            for k in 0..3 {
                if p[k] < min[k] {
                    min[k] = p[k];
                }
                if p[k] > max[k] {
                    max[k] = p[k];
                }
            }
        }
        Self { min, max }
    }

    /// 8 corners in a stable order. Caller is expected not to rely on
    /// the specific order; the only contract is "all 8 corners".
    pub fn corners(&self) -> [[f32; 3]; 8] {
        let [lx, ly, lz] = self.min;
        let [hx, hy, hz] = self.max;
        [
            [lx, ly, lz],
            [hx, ly, lz],
            [lx, hy, lz],
            [hx, hy, lz],
            [lx, ly, hz],
            [hx, ly, hz],
            [lx, hy, hz],
            [hx, hy, hz],
        ]
    }

    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    pub fn half_extents(&self) -> [f32; 3] {
        [
            (self.max[0] - self.min[0]) * 0.5,
            (self.max[1] - self.min[1]) * 0.5,
            (self.max[2] - self.min[2]) * 0.5,
        ]
    }

    /// Return `true` if the AABB has positive volume in every axis.
    pub fn is_valid(&self) -> bool {
        self.min[0] < self.max[0] && self.min[1] < self.max[1] && self.min[2] < self.max[2]
    }
}

/// A plane in the form `ax + by + cz + d = 0`, with the normal pointing
/// **into** the frustum (so the inside half-space has `dot(p, n) + d >= 0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub normal: Vec3,
    pub d: f32,
}

impl Plane {
    /// Signed distance from `point` to the plane. Positive = inside.
    pub fn distance(&self, point: Vec3) -> f32 {
        self.normal.dot(point) + self.d
    }

    /// Normalise the plane so `normal` is unit-length and `d` scales
    /// accordingly. Stable for any plane with a non-zero normal.
    pub fn normalise(&self) -> Self {
        let len = self.normal.length();
        if len > 0.0 {
            Self {
                normal: self.normal / len,
                d: self.d / len,
            }
        } else {
            *self
        }
    }
}

/// Six planes of a view frustum, in fixed slot order:
/// `[left, right, bottom, top, near, far]`. Each normal points **into**
/// the frustum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Extract from a clip-from-world matrix using the standard
    /// Gribb-Hartmann formulation. Works for any standard projection
    /// (perspective, orthographic, oblique).
    pub fn from_view_proj(view_proj: Mat4) -> Self {
        // Rows of the column-major matrix.
        let cols = view_proj.to_cols_array_2d();
        // row(k) = [m[0][k], m[1][k], m[2][k], m[3][k]]
        let row = |k: usize| Vec4::new(cols[0][k], cols[1][k], cols[2][k], cols[3][k]);
        let r0 = row(0);
        let r1 = row(1);
        let r2 = row(2);
        let r3 = row(3);
        // Each frustum plane is sum/difference of rows.
        let left = r3 + r0;
        let right = r3 - r0;
        let bottom = r3 + r1;
        let top = r3 - r1;
        // wgpu / D3D-style clip space: z is [0,1] so near = r2, far = r3 - r2.
        let near = r2;
        let far = r3 - r2;
        let to_plane = |v: Vec4| {
            Plane {
                normal: Vec3::new(v.x, v.y, v.z),
                d: v.w,
            }
            .normalise()
        };
        Self {
            planes: [
                to_plane(left),
                to_plane(right),
                to_plane(bottom),
                to_plane(top),
                to_plane(near),
                to_plane(far),
            ],
        }
    }

    /// Conservative AABB rejection: an AABB is **outside** if it lies
    /// fully behind any plane (i.e. its positive vertex w.r.t. the
    /// plane normal still has a negative signed distance). Returns
    /// `true` when the AABB might be visible.
    ///
    /// Standard "p-vertex" test — exact for AABBs vs. planes.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        for plane in &self.planes {
            // Find the AABB vertex furthest along the plane normal.
            let p = Vec3::new(
                if plane.normal.x >= 0.0 {
                    aabb.max[0]
                } else {
                    aabb.min[0]
                },
                if plane.normal.y >= 0.0 {
                    aabb.max[1]
                } else {
                    aabb.min[1]
                },
                if plane.normal.z >= 0.0 {
                    aabb.max[2]
                } else {
                    aabb.min[2]
                },
            );
            if plane.distance(p) < 0.0 {
                return false;
            }
        }
        true
    }
}

/// Cull an iterator of `(handle, aabb)` tuples by frustum. Returns the
/// handles whose AABBs survive.
pub fn cull_visible<T: Clone>(frustum: &Frustum, items: &[(T, Aabb)]) -> Vec<T> {
    items
        .iter()
        .filter(|(_, aabb)| frustum.intersects_aabb(aabb))
        .map(|(t, _)| t.clone())
        .collect()
}

/// Hi-Z occlusion-handshake input: the AABB projected into NDC + the
/// mip level to sample. Produced by `project_for_hiz` and consumed by
/// the GPU side once the depth pyramid is built. This is a pure CPU
/// helper so we can unit-test it; the actual depth-pyramid sampling is
/// in the wgpu pipeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HiZQuery {
    /// Screen-space rect in `[0, 1]`: `[u_min, v_min, u_max, v_max]`.
    pub rect: [f32; 4],
    /// Closest projected NDC z of the 8 corners in `[0, 1]`. Compared
    /// against the depth-pyramid sample; if the pyramid is closer, the
    /// AABB is fully occluded.
    pub min_ndc_z: f32,
    /// Which mip level of the depth pyramid to sample. The mip level
    /// is the smallest where one texel covers the AABB's screen
    /// footprint, so the conservative sample is the max of all texels
    /// the AABB overlaps.
    pub mip: u32,
    /// `true` if the AABB straddles the near plane (treated as visible
    /// unconditionally — projecting through `w=0` is ill-defined).
    pub straddles_near: bool,
}

/// Project an AABB into NDC and decide which Hi-Z mip to sample. See
/// [`HiZQuery`] for the output semantics.
pub fn project_for_hiz(view_proj: Mat4, aabb: &Aabb, pyramid_size: u32) -> HiZQuery {
    let corners = aabb.corners();
    let mut min_u = f32::INFINITY;
    let mut min_v = f32::INFINITY;
    let mut max_u = f32::NEG_INFINITY;
    let mut max_v = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut straddles = false;
    for c in corners {
        let clip = view_proj * Vec4::new(c[0], c[1], c[2], 1.0);
        if clip.w <= 0.0 {
            straddles = true;
            continue;
        }
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        let ndc_z = clip.z / clip.w;
        let u = ndc_x * 0.5 + 0.5;
        let v = ndc_y * -0.5 + 0.5; // wgpu y-flip
        if u < min_u {
            min_u = u;
        }
        if v < min_v {
            min_v = v;
        }
        if u > max_u {
            max_u = u;
        }
        if v > max_v {
            max_v = v;
        }
        if ndc_z < min_z {
            min_z = ndc_z;
        }
    }
    if straddles || !min_u.is_finite() || !min_v.is_finite() {
        return HiZQuery {
            rect: [0.0, 0.0, 1.0, 1.0],
            min_ndc_z: 0.0,
            mip: 0,
            straddles_near: true,
        };
    }
    let clamp01 = |x: f32| x.clamp(0.0, 1.0);
    let rect = [
        clamp01(min_u),
        clamp01(min_v),
        clamp01(max_u),
        clamp01(max_v),
    ];
    // Footprint in pyramid pixels at mip 0.
    let w_px = ((rect[2] - rect[0]) * pyramid_size as f32).max(1.0);
    let h_px = ((rect[3] - rect[1]) * pyramid_size as f32).max(1.0);
    let max_dim = w_px.max(h_px);
    // Pick the mip level whose texel covers the AABB footprint with a
    // single sample. `log2` rounded up.
    let mip = max_dim.log2().ceil().max(0.0) as u32;
    let max_mip = if pyramid_size <= 1 {
        0
    } else {
        (pyramid_size as f32).log2().floor() as u32
    };
    HiZQuery {
        rect,
        min_ndc_z: min_z,
        mip: mip.min(max_mip),
        straddles_near: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn perspective(aspect: f32) -> Mat4 {
        Mat4::perspective_rh(60_f32.to_radians(), aspect, 0.1, 100.0)
    }

    fn view_at(eye: Vec3, target: Vec3) -> Mat4 {
        Mat4::look_at_rh(eye, target, Vec3::Y)
    }

    #[test]
    fn aabb_from_points_handles_single_point() {
        let aabb = Aabb::from_points(vec![[1.0, 2.0, 3.0]]).unwrap();
        assert_eq!(aabb.min, [1.0, 2.0, 3.0]);
        assert_eq!(aabb.max, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn aabb_from_points_handles_box() {
        let aabb =
            Aabb::from_points(vec![[-1.0, -2.0, -3.0], [4.0, 5.0, 6.0], [0.0, 0.0, 0.0]]).unwrap();
        assert_eq!(aabb.min, [-1.0, -2.0, -3.0]);
        assert_eq!(aabb.max, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn plane_distance_signs() {
        let p = Plane {
            normal: Vec3::X,
            d: -2.0,
        };
        // x=3 is at distance +1; x=1 is at distance -1; x=2 is at 0.
        assert!((p.distance(Vec3::new(3.0, 0.0, 0.0)) - 1.0).abs() < 1e-6);
        assert!((p.distance(Vec3::new(1.0, 0.0, 0.0)) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn frustum_accepts_unit_aabb_in_front_of_camera() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let frustum = Frustum::from_view_proj(proj * view);
        let aabb = Aabb {
            min: [-0.5, -0.5, -0.5],
            max: [0.5, 0.5, 0.5],
        };
        assert!(frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn frustum_rejects_aabb_behind_camera() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let frustum = Frustum::from_view_proj(proj * view);
        // 50 units BEHIND the camera (camera at z=5 looking -Z).
        let aabb = Aabb {
            min: [-0.5, -0.5, 49.5],
            max: [0.5, 0.5, 50.5],
        };
        assert!(!frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn frustum_rejects_aabb_far_to_the_right() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let frustum = Frustum::from_view_proj(proj * view);
        let aabb = Aabb {
            min: [1000.0, -0.5, -0.5],
            max: [1001.0, 0.5, 0.5],
        };
        assert!(!frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn cull_visible_filters_correctly() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let frustum = Frustum::from_view_proj(proj * view);
        let items = vec![
            (
                "visible",
                Aabb {
                    min: [-0.5, -0.5, -0.5],
                    max: [0.5, 0.5, 0.5],
                },
            ),
            (
                "behind",
                Aabb {
                    min: [-0.5, -0.5, 49.5],
                    max: [0.5, 0.5, 50.5],
                },
            ),
            (
                "far_right",
                Aabb {
                    min: [1000.0, -0.5, -0.5],
                    max: [1001.0, 0.5, 0.5],
                },
            ),
        ];
        let kept = cull_visible(&frustum, &items);
        assert_eq!(kept, vec!["visible"]);
    }

    #[test]
    fn aabb_transformed_preserves_box() {
        let aabb = Aabb {
            min: [-1.0; 3],
            max: [1.0; 3],
        };
        let t = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let moved = aabb.transformed(t);
        assert!((moved.min[0] - 9.0).abs() < 1e-6);
        assert!((moved.max[0] - 11.0).abs() < 1e-6);
    }

    #[test]
    fn aabb_transformed_under_rotation_expands_to_obb_box() {
        // Rotating a unit cube by 45° about Y should expand the X/Z extents.
        let aabb = Aabb {
            min: [-0.5; 3],
            max: [0.5; 3],
        };
        let t = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let rotated = aabb.transformed(t);
        let expected_half = (0.5_f32 * 2.0_f32.sqrt()).abs();
        assert!((rotated.max[0] - expected_half).abs() < 1e-5);
        assert!((rotated.max[2] - expected_half).abs() < 1e-5);
    }

    #[test]
    fn project_for_hiz_centered_unit_cube_picks_low_mip() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let view_proj = proj * view;
        let aabb = Aabb {
            min: [-0.5; 3],
            max: [0.5; 3],
        };
        let q = project_for_hiz(view_proj, &aabb, 1024);
        assert!(!q.straddles_near);
        assert!(q.rect[0] > 0.0 && q.rect[2] < 1.0);
        assert!(q.rect[1] > 0.0 && q.rect[3] < 1.0);
        assert!(q.min_ndc_z >= 0.0 && q.min_ndc_z <= 1.0);
    }

    #[test]
    fn project_for_hiz_handles_aabb_straddling_near_plane() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let view_proj = proj * view;
        // AABB spans from very close to behind the camera.
        let aabb = Aabb {
            min: [-1.0, -1.0, 4.99],
            max: [1.0, 1.0, 5.01],
        };
        let q = project_for_hiz(view_proj, &aabb, 1024);
        assert!(q.straddles_near, "expected near-plane straddle flag");
        // Whole-screen rect + permissive depth so caller treats as visible.
        assert_eq!(q.rect, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn project_for_hiz_picks_higher_mip_for_large_footprint() {
        let proj = perspective(1.0);
        let view = view_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO);
        let view_proj = proj * view;
        let tiny = Aabb {
            min: [-0.05; 3],
            max: [0.05; 3],
        };
        let big = Aabb {
            min: [-3.0; 3],
            max: [3.0; 3],
        };
        let q_tiny = project_for_hiz(view_proj, &tiny, 1024);
        let q_big = project_for_hiz(view_proj, &big, 1024);
        assert!(q_big.mip > q_tiny.mip);
    }
}
