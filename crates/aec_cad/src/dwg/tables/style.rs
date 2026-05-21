//! STYLE / SHAPEFILE table record codec.

/// Text style record.  Each TEXT/MTEXT entity references one of these
/// by handle for its font / oblique / width-factor settings.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleRecord {
    pub name: String,
    pub font_file: String,
    pub fixed_height: f64,
    pub width_factor: f64,
    pub oblique_angle: f64,
    pub vertical: bool,
    pub shape_file: bool,
}

impl Default for StyleRecord {
    fn default() -> Self {
        Self {
            name: "Standard".to_string(),
            font_file: "txt.shx".to_string(),
            fixed_height: 0.0,
            width_factor: 1.0,
            oblique_angle: 0.0,
            vertical: false,
            shape_file: false,
        }
    }
}
