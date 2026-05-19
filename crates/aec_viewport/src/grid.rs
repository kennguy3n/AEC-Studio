//! Infinite ground grid (works in both 3D and 2D modes).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridStyle {
    pub minor_spacing_mm: f32,
    pub major_every: u32,
    pub minor_color_rgba: [f32; 4],
    pub major_color_rgba: [f32; 4],
}

impl Default for GridStyle {
    fn default() -> Self {
        Self {
            minor_spacing_mm: 100.0,
            major_every: 10,
            minor_color_rgba: [0.85, 0.85, 0.88, 0.4],
            major_color_rgba: [0.70, 0.70, 0.75, 0.7],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub style: GridStyle,
    pub visible: bool,
}

impl Grid {
    pub fn new() -> Self {
        Self {
            style: GridStyle::default(),
            visible: true,
        }
    }

    /// Snap a world-space X/Z point to the nearest minor grid intersection.
    pub fn snap_xz(&self, x: f32, z: f32) -> (f32, f32) {
        let s = self.style.minor_spacing_mm;
        ((x / s).round() * s, (z / s).round() * s)
    }

    /// Major spacing in mm.
    pub fn major_spacing_mm(&self) -> f32 {
        self.style.minor_spacing_mm * self.style.major_every as f32
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_xz_rounds_to_minor() {
        let g = Grid::new();
        let (x, z) = g.snap_xz(123.4, 456.8);
        assert_eq!(x, 100.0);
        assert_eq!(z, 500.0);
    }

    #[test]
    fn major_spacing_is_minor_times_every() {
        let g = Grid::new();
        assert!((g.major_spacing_mm() - 1000.0).abs() < f32::EPSILON);
    }
}
