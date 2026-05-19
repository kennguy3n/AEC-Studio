//! Saved camera shared between the viewport and the render worker.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Perspective3d,
    Orthographic2d,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedCamera {
    pub id: EntityId,
    pub name: String,
    pub mode: CameraMode,
    pub position_mm: [f64; 3],
    pub target_mm: [f64; 3],
    pub focal_length_mm: f64,
    pub exposure_ev: f64,
    pub white_balance_k: u32,
    pub depth_of_field_f: Option<f64>,
    pub aspect_ratio: f64,
    /// Ortho width in mm (for 2D mode); ignored in 3D.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ortho_width_mm: Option<f64>,
}

impl SavedCamera {
    /// Compute horizontal field of view in radians from focal length
    /// (35mm-equivalent: sensor width = 36mm).
    pub fn horizontal_fov(&self) -> f64 {
        let sensor_width_mm = 36.0;
        2.0 * (sensor_width_mm / (2.0 * self.focal_length_mm)).atan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fov_inverts_with_focal_length() {
        let mut cam = SavedCamera {
            id: EntityId::new(),
            name: "T".into(),
            mode: CameraMode::Perspective3d,
            position_mm: [0.0, 0.0, 0.0],
            target_mm: [0.0, 1000.0, 0.0],
            focal_length_mm: 50.0,
            exposure_ev: 0.0,
            white_balance_k: 5500,
            depth_of_field_f: None,
            aspect_ratio: 1.0,
            ortho_width_mm: None,
        };
        let fov_50 = cam.horizontal_fov();
        cam.focal_length_mm = 24.0;
        let fov_24 = cam.horizontal_fov();
        assert!(fov_24 > fov_50);
    }
}
