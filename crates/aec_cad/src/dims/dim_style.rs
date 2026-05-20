//! Dimension style — controls the visual presentation of dimensions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowType {
    ClosedFilled,
    OpenFilled,
    Architectural,
    Oblique,
    Dot,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimUnit {
    Decimal,
    Scientific,
    EngineeringFeetInches,
    ArchitecturalFeetInches,
    Fractional,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimStyle {
    pub name: String,
    pub arrow_type: ArrowType,
    pub arrow_size: f64,
    pub text_height: f64,
    /// Offset of dim text above the dim line.
    pub text_offset: f64,
    /// Distance from the dim point to the extension line start.
    pub extension_offset: f64,
    /// Length the extension line projects beyond the dim line.
    pub extension_above_dim: f64,
    /// Number of decimal places for the dimension value.
    pub precision: u8,
    /// Linear unit.
    pub unit: DimUnit,
    /// Tolerance limit display (`±tol`).
    pub tolerance: Option<f64>,
    pub suppress_leading_zero: bool,
    pub suppress_trailing_zero: bool,
    pub show_alternate_units: bool,
    pub alternate_scale: f64,
}

impl Default for DimStyle {
    fn default() -> Self {
        Self {
            name: "STANDARD".into(),
            arrow_type: ArrowType::ClosedFilled,
            arrow_size: 2.5,
            text_height: 2.5,
            text_offset: 0.625,
            extension_offset: 0.625,
            extension_above_dim: 1.25,
            precision: 2,
            unit: DimUnit::Decimal,
            tolerance: None,
            suppress_leading_zero: false,
            suppress_trailing_zero: true,
            show_alternate_units: false,
            alternate_scale: 25.4,
        }
    }
}

impl DimStyle {
    pub fn format_distance(&self, value: f64) -> String {
        match self.unit {
            DimUnit::Decimal | DimUnit::Scientific => self.format_decimal(value),
            DimUnit::EngineeringFeetInches | DimUnit::ArchitecturalFeetInches => {
                self.format_feet_inches(value)
            }
            DimUnit::Fractional => self.format_fractional(value),
        }
    }

    fn format_decimal(&self, value: f64) -> String {
        let mut s = format!("{value:.*}", self.precision as usize);
        if self.suppress_trailing_zero && s.contains('.') {
            s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        if self.suppress_leading_zero {
            if let Some(rest) = s.strip_prefix("0.") {
                s = format!(".{rest}");
            }
        }
        s
    }

    fn format_feet_inches(&self, value_mm: f64) -> String {
        // Treat input as millimetres; convert to feet+inches.
        let inches_total = value_mm / 25.4;
        let feet = (inches_total / 12.0).trunc() as i64;
        let inches = inches_total - (feet as f64) * 12.0;
        format!("{feet}'-{inches:.*}\"", self.precision as usize)
    }

    fn format_fractional(&self, value: f64) -> String {
        let whole = value.trunc() as i64;
        let frac = (value - whole as f64).abs();
        let denom = 1u32 << self.precision.min(8);
        let num = (frac * denom as f64).round() as u32;
        if num == 0 {
            format!("{whole}")
        } else if num == denom {
            format!("{}", whole + 1)
        } else {
            format!("{whole} {num}/{denom}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_precision() {
        let style = DimStyle::default();
        assert_eq!(style.format_distance(1234.5678), "1234.57");
    }

    #[test]
    fn trailing_zero_suppressed() {
        let style = DimStyle::default();
        assert_eq!(style.format_distance(1.00), "1");
        assert_eq!(style.format_distance(1.5), "1.5");
    }

    #[test]
    fn feet_inches_format() {
        let style = DimStyle {
            unit: DimUnit::ArchitecturalFeetInches,
            precision: 1,
            ..DimStyle::default()
        };
        let out = style.format_distance(914.4); // = 3' exactly
        assert!(out.starts_with("3'"));
    }

    #[test]
    fn fractional_eight() {
        let style = DimStyle {
            unit: DimUnit::Fractional,
            precision: 3,
            ..DimStyle::default()
        };
        assert_eq!(style.format_distance(2.5), "2 4/8");
    }
}
