//! Ortho mode — constrain motion to the dominant axis.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrthoMode {
    pub enabled: bool,
}

impl OrthoMode {
    pub fn apply(self, anchor: [f64; 2], point: [f64; 2]) -> [f64; 2] {
        if !self.enabled {
            return point;
        }
        let dx = (point[0] - anchor[0]).abs();
        let dy = (point[1] - anchor[1]).abs();
        if dx >= dy {
            [point[0], anchor[1]]
        } else {
            [anchor[0], point[1]]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_passes_through() {
        let o = OrthoMode { enabled: false };
        assert_eq!(o.apply([0.0, 0.0], [3.0, 4.0]), [3.0, 4.0]);
    }

    #[test]
    fn snaps_to_dominant_axis() {
        let o = OrthoMode { enabled: true };
        assert_eq!(o.apply([0.0, 0.0], [10.0, 2.0]), [10.0, 0.0]);
        assert_eq!(o.apply([0.0, 0.0], [2.0, 10.0]), [0.0, 10.0]);
    }
}
