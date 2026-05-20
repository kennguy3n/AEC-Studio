//! Polar tracking — snap to angle increments from an anchor.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolarTracking {
    pub enabled: bool,
    /// Increment in degrees (e.g. 15.0, 30.0, 45.0, 90.0).
    pub increment_deg: f64,
    /// Additional explicit angles to snap to in degrees.
    pub additional_angles_deg: Vec<f64>,
    /// Snap aperture (degrees) — within this delta from a snap line we
    /// snap, otherwise we pass the input through.
    pub aperture_deg: f64,
}

impl Default for PolarTracking {
    fn default() -> Self {
        Self {
            enabled: false,
            increment_deg: 15.0,
            additional_angles_deg: Vec::new(),
            aperture_deg: 1.0,
        }
    }
}

impl PolarTracking {
    pub fn apply(&self, anchor: [f64; 2], point: [f64; 2]) -> [f64; 2] {
        if !self.enabled {
            return point;
        }
        let dx = point[0] - anchor[0];
        let dy = point[1] - anchor[1];
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < f64::EPSILON {
            return anchor;
        }
        let angle = dy.atan2(dx).to_degrees();
        let mut best_angle = angle;
        let mut best_delta = f64::INFINITY;
        if self.increment_deg > 0.0 {
            let snapped = (angle / self.increment_deg).round() * self.increment_deg;
            let delta = angular_delta(angle, snapped);
            if delta.abs() <= self.aperture_deg && delta.abs() < best_delta.abs() {
                best_angle = snapped;
                best_delta = delta;
            }
        }
        for &add in &self.additional_angles_deg {
            let delta = angular_delta(angle, add);
            if delta.abs() <= self.aperture_deg && delta.abs() < best_delta.abs() {
                best_angle = add;
                best_delta = delta;
            }
        }
        if best_delta.is_finite() {
            let rad = best_angle.to_radians();
            [anchor[0] + rad.cos() * dist, anchor[1] + rad.sin() * dist]
        } else {
            point
        }
    }
}

fn angular_delta(a: f64, b: f64) -> f64 {
    let mut d = a - b;
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_horizontal() {
        let p = PolarTracking {
            enabled: true,
            increment_deg: 90.0,
            aperture_deg: 5.0,
            ..Default::default()
        };
        let r = p.apply([0.0, 0.0], [10.0, 0.5]);
        assert!((r[1] - 0.0).abs() < 1e-6);
        assert!((r[0] - 10.012).abs() < 0.05);
    }

    #[test]
    fn snap_to_additional_angle() {
        let p = PolarTracking {
            enabled: true,
            increment_deg: 90.0,
            aperture_deg: 5.0,
            additional_angles_deg: vec![30.0],
        };
        // input around 31° — should snap to 30°.
        let r = p.apply(
            [0.0, 0.0],
            [
                10.0 * 31.0_f64.to_radians().cos(),
                10.0 * 31.0_f64.to_radians().sin(),
            ],
        );
        let theta = r[1].atan2(r[0]).to_degrees();
        assert!((theta - 30.0).abs() < 1e-3);
    }

    #[test]
    fn outside_aperture_passes_through() {
        let p = PolarTracking {
            enabled: true,
            increment_deg: 90.0,
            aperture_deg: 1.0,
            ..Default::default()
        };
        // 30° is well outside the 1° aperture from the nearest 0/90.
        let r = p.apply(
            [0.0, 0.0],
            [
                10.0 * 30.0_f64.to_radians().cos(),
                10.0 * 30.0_f64.to_radians().sin(),
            ],
        );
        let theta = r[1].atan2(r[0]).to_degrees();
        assert!((theta - 30.0).abs() < 1e-6);
    }
}
