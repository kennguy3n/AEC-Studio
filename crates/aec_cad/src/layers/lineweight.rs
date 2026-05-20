//! Standard ISO lineweights — millimetres encoded as hundredths-of-mm.
//!
//! AutoCAD stores lineweights as `i16` values where `25 == 0.25 mm`, with
//! the special sentinels `-3 = ByLayer`, `-2 = ByBlock`, `-1 = Default`.
//! We use the same encoding for DXF interop.

use serde::{Deserialize, Serialize};

/// Standard ISO lineweights (mm). AutoCAD recognises this exact set.
pub const STANDARD_LINEWEIGHTS_MM: &[f64] = &[
    0.00, 0.05, 0.09, 0.13, 0.15, 0.18, 0.20, 0.25, 0.30, 0.35, 0.40, 0.50, 0.53, 0.60, 0.70, 0.80,
    0.90, 1.00, 1.06, 1.20, 1.40, 1.58, 2.00, 2.11,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Lineweight(pub i16);

impl Lineweight {
    pub const BYLAYER: Lineweight = Lineweight(-3);
    pub const BYBLOCK: Lineweight = Lineweight(-2);
    pub const DEFAULT: Lineweight = Lineweight(-1);

    /// Build a lineweight from mm, snapping to the nearest standard step.
    pub fn from_mm(mm: f64) -> Self {
        let mut best = STANDARD_LINEWEIGHTS_MM[0];
        let mut best_d = f64::INFINITY;
        for &w in STANDARD_LINEWEIGHTS_MM {
            let d = (w - mm).abs();
            if d < best_d {
                best_d = d;
                best = w;
            }
        }
        Self((best * 100.0).round() as i16)
    }

    /// Build a lineweight from mm without snapping. Useful for plot-style
    /// tables that allow arbitrary widths.
    pub fn from_mm_raw(mm: f64) -> Self {
        Self((mm * 100.0).round() as i16)
    }

    pub fn to_mm(self) -> Option<f64> {
        if self.0 < 0 {
            None
        } else {
            Some(f64::from(self.0) / 100.0)
        }
    }

    pub fn is_special(self) -> bool {
        self.0 < 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_nearest_standard() {
        // 0.24 → snap to 0.25 (closest standard step).
        assert_eq!(Lineweight::from_mm(0.24).0, 25);
        // 0.97 → 1.00 (unambiguous; 0.95 sits midway between 0.90 and 1.00).
        assert_eq!(Lineweight::from_mm(0.97).0, 100);
        // 0.95 ties to the lower of {0.90, 1.00} per first-match.
        assert_eq!(Lineweight::from_mm(0.95).0, 90);
    }

    #[test]
    fn special_values_have_no_mm() {
        assert!(Lineweight::BYLAYER.to_mm().is_none());
        assert!(Lineweight::DEFAULT.to_mm().is_none());
    }

    #[test]
    fn raw_value_passes_through() {
        assert_eq!(Lineweight::from_mm_raw(0.123).0, 12);
    }
}
