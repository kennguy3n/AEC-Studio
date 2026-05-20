//! Grid + grid snap.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridSpec {
    pub spacing: [f64; 2],
    pub subdivisions: u32,
    pub visible: bool,
    pub snap_enabled: bool,
}

impl Default for GridSpec {
    fn default() -> Self {
        Self {
            spacing: [10.0, 10.0],
            subdivisions: 5,
            visible: true,
            snap_enabled: true,
        }
    }
}

impl GridSpec {
    pub fn new(spacing_x: f64, spacing_y: f64) -> Self {
        Self {
            spacing: [spacing_x.max(1e-9), spacing_y.max(1e-9)],
            ..Default::default()
        }
    }

    pub fn snap(&self, point: [f64; 2]) -> [f64; 2] {
        if !self.snap_enabled {
            return point;
        }
        let sx = self.spacing[0];
        let sy = self.spacing[1];
        [(point[0] / sx).round() * sx, (point[1] / sy).round() * sy]
    }

    /// Compute the set of horizontal/vertical grid lines visible inside
    /// the given world-space rectangle.
    pub fn visible_lines(&self, min: [f64; 2], max: [f64; 2]) -> GridLines {
        let sx = self.spacing[0];
        let sy = self.spacing[1];
        let i_min_x = (min[0] / sx).floor() as i64;
        let i_max_x = (max[0] / sx).ceil() as i64;
        let i_min_y = (min[1] / sy).floor() as i64;
        let i_max_y = (max[1] / sy).ceil() as i64;
        let mut majors_x = Vec::with_capacity((i_max_x - i_min_x + 1).max(0) as usize);
        let mut minors_x = Vec::new();
        for i in i_min_x..=i_max_x {
            let x = i as f64 * sx;
            if self.subdivisions > 0 && i.rem_euclid(self.subdivisions as i64) == 0 {
                majors_x.push(x);
            } else {
                minors_x.push(x);
            }
        }
        let mut majors_y = Vec::with_capacity((i_max_y - i_min_y + 1).max(0) as usize);
        let mut minors_y = Vec::new();
        for j in i_min_y..=i_max_y {
            let y = j as f64 * sy;
            if self.subdivisions > 0 && j.rem_euclid(self.subdivisions as i64) == 0 {
                majors_y.push(y);
            } else {
                minors_y.push(y);
            }
        }
        GridLines {
            majors_x,
            minors_x,
            majors_y,
            minors_y,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridLines {
    pub majors_x: Vec<f64>,
    pub minors_x: Vec<f64>,
    pub majors_y: Vec<f64>,
    pub minors_y: Vec<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_rounds_to_nearest_step() {
        let g = GridSpec::new(10.0, 10.0);
        assert_eq!(g.snap([12.3, -4.5]), [10.0, -0.0]);
        assert_eq!(g.snap([16.0, 16.0]), [20.0, 20.0]);
    }

    #[test]
    fn visible_lines_include_origin() {
        let g = GridSpec::new(10.0, 10.0);
        let lines = g.visible_lines([-5.0, -5.0], [25.0, 25.0]);
        assert!(lines.majors_x.contains(&0.0));
        assert!(lines.majors_y.contains(&0.0));
    }

    #[test]
    fn subdivisions_split_majors_and_minors() {
        let mut g = GridSpec::new(1.0, 1.0);
        g.subdivisions = 5;
        let lines = g.visible_lines([0.0, 0.0], [10.0, 0.0]);
        // 0, 5, 10 are majors; 1..4, 6..9 are minors.
        assert!(lines.majors_x.contains(&5.0));
        assert!(lines.minors_x.contains(&3.0));
    }
}
