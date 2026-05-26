//! DIMSTYLE table record codec.
//!
//! AutoCAD DIMSTYLEs carry ~70 system variables (DIMASZ, DIMTXT, DIMLFAC, …).
//! We round-trip the subset that the existing [`crate::dxf::DxfDimStyle`]
//! exposes (text height, arrow size, units scale) and preserve any
//! extra vars opaquely as `(varname, value)` pairs.

use crate::dxf::DxfDimStyle;

#[derive(Debug, Clone, PartialEq)]
pub struct DimStyleRecord {
    pub name: String,
    pub text_height: f64,
    pub arrow_size: f64,
    pub units_scale: f64,
    pub decimal_places: u8,
    pub text_style: String,
    /// Vars that don't have a structured place in `DxfDimStyle` yet.
    /// Stored as `(name, value_as_f64)` pairs so an unmodified write
    /// can round-trip them without loss.
    pub extra_vars: Vec<(String, f64)>,
}

impl DimStyleRecord {
    pub fn into_dxf(self) -> DxfDimStyle {
        DxfDimStyle {
            name: self.name,
            text_height: self.text_height,
            arrow_size: self.arrow_size,
            units_scale: self.units_scale,
            decimal_places: self.decimal_places,
            text_style: self.text_style,
        }
    }

    pub fn from_dxf(dxf: &DxfDimStyle) -> Self {
        Self {
            name: dxf.name.clone(),
            text_height: dxf.text_height,
            arrow_size: dxf.arrow_size,
            units_scale: dxf.units_scale,
            decimal_places: dxf.decimal_places,
            text_style: dxf.text_style.clone(),
            extra_vars: Vec::new(),
        }
    }
}
