//! Sheet definition — paper, orientation, margins, title block reference.

use serde::{Deserialize, Serialize};

use crate::sheets::title_block::TitleBlock;
use crate::sheets::viewport::SheetViewport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PaperSize {
    IsoA0,
    IsoA1,
    IsoA2,
    IsoA3,
    IsoA4,
    AnsiA,
    AnsiB,
    AnsiC,
    AnsiD,
    AnsiE,
    Custom(u32, u32),
}

impl PaperSize {
    /// Returns the (width, height) in millimetres at portrait orientation.
    pub fn dimensions_mm(&self) -> (f64, f64) {
        match self {
            PaperSize::IsoA0 => (841.0, 1189.0),
            PaperSize::IsoA1 => (594.0, 841.0),
            PaperSize::IsoA2 => (420.0, 594.0),
            PaperSize::IsoA3 => (297.0, 420.0),
            PaperSize::IsoA4 => (210.0, 297.0),
            // ANSI sizes (in inches → mm).
            PaperSize::AnsiA => (8.5 * 25.4, 11.0 * 25.4),
            PaperSize::AnsiB => (11.0 * 25.4, 17.0 * 25.4),
            PaperSize::AnsiC => (17.0 * 25.4, 22.0 * 25.4),
            PaperSize::AnsiD => (22.0 * 25.4, 34.0 * 25.4),
            PaperSize::AnsiE => (34.0 * 25.4, 44.0 * 25.4),
            PaperSize::Custom(w, h) => (*w as f64, *h as f64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Margins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Default for Margins {
    fn default() -> Self {
        Self {
            top: 10.0,
            right: 10.0,
            bottom: 10.0,
            left: 25.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub name: String,
    pub paper: PaperSize,
    pub orientation: Orientation,
    pub margins: Margins,
    pub title_block: Option<TitleBlock>,
    pub viewports: Vec<SheetViewport>,
}

impl Sheet {
    pub fn new(name: impl Into<String>, paper: PaperSize) -> Self {
        Self {
            name: name.into(),
            paper,
            orientation: Orientation::Landscape,
            margins: Margins::default(),
            title_block: None,
            viewports: Vec::new(),
        }
    }

    /// Returns sheet (width, height) accounting for orientation.
    pub fn dimensions_mm(&self) -> (f64, f64) {
        let (w, h) = self.paper.dimensions_mm();
        match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        }
    }

    pub fn printable_area_mm(&self) -> (f64, f64) {
        let (w, h) = self.dimensions_mm();
        (
            (w - self.margins.left - self.margins.right).max(0.0),
            (h - self.margins.top - self.margins.bottom).max(0.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a3_landscape_dimensions() {
        let s = Sheet::new("Plan", PaperSize::IsoA3);
        assert_eq!(s.dimensions_mm(), (420.0, 297.0));
    }

    #[test]
    fn a4_portrait_dimensions() {
        let mut s = Sheet::new("Cover", PaperSize::IsoA4);
        s.orientation = Orientation::Portrait;
        assert_eq!(s.dimensions_mm(), (210.0, 297.0));
    }

    #[test]
    fn ansi_d_dimensions() {
        let s = Sheet::new("D Sheet", PaperSize::AnsiD);
        let (w, h) = s.dimensions_mm();
        assert!((w - 34.0 * 25.4).abs() < 1e-6);
        assert!((h - 22.0 * 25.4).abs() < 1e-6);
    }

    #[test]
    fn printable_area_subtracts_margins() {
        let s = Sheet::new("Plan", PaperSize::IsoA3);
        let (pw, ph) = s.printable_area_mm();
        assert!((pw - (420.0 - 10.0 - 25.0)).abs() < 1e-9);
        assert!((ph - (297.0 - 10.0 - 10.0)).abs() < 1e-9);
    }
}
