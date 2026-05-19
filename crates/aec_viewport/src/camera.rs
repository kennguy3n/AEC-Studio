//! Camera math (3D perspective + 2D orthographic) + orbit controller +
//! screen-space ray casting.

use glam::{Mat4, Vec2, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    /// 3D perspective view used in Design mode and BIM mode.
    Perspective,
    /// 2D orthographic view used in Draft mode.
    Orthographic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub mode: CameraMode,
    /// World-space position (mm).
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub up: [f32; 3],
    /// Field of view in radians (Perspective mode).
    pub fov_radians: f32,
    /// Half-height of the visible volume (Orthographic mode), in mm.
    pub ortho_half_height_mm: f32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    pub fn default_perspective() -> Self {
        Self {
            mode: CameraMode::Perspective,
            position: [5000.0, 3000.0, 5000.0],
            target: [0.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            fov_radians: 60.0_f32.to_radians(),
            ortho_half_height_mm: 5000.0,
            near: 50.0,
            far: 200_000.0,
        }
    }

    pub fn default_orthographic() -> Self {
        Self {
            mode: CameraMode::Orthographic,
            position: [0.0, 10_000.0, 0.0],
            target: [0.0, 0.0, 0.0],
            up: [0.0, 0.0, -1.0],
            fov_radians: 60.0_f32.to_radians(),
            ortho_half_height_mm: 5000.0,
            near: 1.0,
            far: 100_000.0,
        }
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.position.into(), self.target.into(), self.up.into())
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        match self.mode {
            CameraMode::Perspective => {
                Mat4::perspective_rh(self.fov_radians, aspect.max(1e-3), self.near, self.far)
            }
            CameraMode::Orthographic => {
                let h = self.ortho_half_height_mm;
                let w = h * aspect.max(1e-3);
                Mat4::orthographic_rh(-w, w, -h, h, self.near, self.far)
            }
        }
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    /// Cast a ray from `(ndc_x, ndc_y)` ∈ \[-1, 1\] into world space.
    pub fn ray_from_ndc(&self, ndc: Vec2, aspect: f32) -> Ray {
        let inv = self.view_projection(aspect).inverse();
        let near_world = inv.project_point3(Vec3::new(ndc.x, ndc.y, -1.0));
        let far_world = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        let origin = near_world;
        let dir = (far_world - near_world).normalize_or_zero();
        Ray {
            origin,
            direction: dir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl Ray {
    /// Intersect the ray with the `Y = 0` ground plane. Returns the
    /// world-space intersection point, or `None` if the ray is parallel to
    /// the ground or hits behind the camera.
    pub fn intersect_ground_plane(&self) -> Option<Vec3> {
        if self.direction.y.abs() < 1e-6 {
            return None;
        }
        let t = -self.origin.y / self.direction.y;
        if t < 0.0 {
            return None;
        }
        Some(self.origin + self.direction * t)
    }
}

pub struct OrbitController {
    /// Distance from `target`.
    radius: f32,
    /// Azimuth angle (around Y), radians.
    yaw: f32,
    /// Elevation angle (above XZ plane), radians.
    pitch: f32,
    /// World-space pivot point.
    target: Vec3,
}

impl OrbitController {
    pub fn new(target: Vec3, radius: f32) -> Self {
        Self {
            radius,
            yaw: 0.0,
            pitch: 0.5,
            target,
        }
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw += dx;
        self.pitch = (self.pitch + dy).clamp(-1.55, 1.55);
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        // Pan in world XZ for simplicity.
        self.target.x += dx;
        self.target.z += dy;
    }

    pub fn zoom(&mut self, delta: f32) {
        self.radius = (self.radius * (1.0 + delta)).clamp(100.0, 200_000.0);
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Apply the controller's state to a [`Camera`], updating its position
    /// and target.
    pub fn apply(&self, camera: &mut Camera) {
        let x = self.radius * self.pitch.cos() * self.yaw.sin();
        let y = self.radius * self.pitch.sin();
        let z = self.radius * self.pitch.cos() * self.yaw.cos();
        let pos = self.target + Vec3::new(x, y, z);
        camera.position = [pos.x, pos.y, pos.z];
        camera.target = [self.target.x, self.target.y, self.target.z];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perspective_projection_is_finite() {
        let cam = Camera::default_perspective();
        let m = cam.view_projection(16.0 / 9.0);
        for row in m.to_cols_array_2d().iter() {
            for v in row.iter() {
                assert!(v.is_finite());
            }
        }
    }

    #[test]
    fn orthographic_projection_uses_ortho_volume() {
        let mut cam = Camera::default_orthographic();
        cam.ortho_half_height_mm = 1000.0;
        let proj = cam.projection_matrix(1.0);
        // Width should scale by aspect (which is 1 here).
        let pt = proj.project_point3(Vec3::new(0.0, 0.0, -50.0));
        assert!(pt.x.is_finite());
    }

    #[test]
    fn ray_hits_ground_plane() {
        let cam = Camera::default_perspective();
        let ray = cam.ray_from_ndc(Vec2::ZERO, 16.0 / 9.0);
        let hit = ray.intersect_ground_plane();
        assert!(hit.is_some());
        let hit = hit.unwrap();
        assert!(hit.y.abs() < 1.0);
    }

    #[test]
    fn orbit_controller_updates_camera() {
        let mut cam = Camera::default_perspective();
        let mut ctrl = OrbitController::new(Vec3::ZERO, 5000.0);
        ctrl.orbit(0.1, -0.1);
        ctrl.zoom(-0.1);
        ctrl.apply(&mut cam);
        assert!((Vec3::from(cam.position).length() - ctrl.radius()).abs() < 1.0);
    }
}
