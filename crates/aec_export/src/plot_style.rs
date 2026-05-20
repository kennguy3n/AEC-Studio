//! Plot style table (CTB/STB equivalent).
//!
//! Maps DXF AutoCAD Color Index (ACI, 1–255) entries to a printed
//! presentation: RGB colour, lineweight (mm), screening (0–100 %),
//! linetype override, and "draw with object linetype" toggle.
//!
//! A `PlotStyleTable` is a deterministic, ordered map keyed on ACI;
//! entries can be added, looked up, and reflected into a deterministic
//! sort order for export.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single plot-style entry — what a given pen colour prints as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlotStyle {
    /// AutoCAD Color Index this entry applies to (1–255).
    pub aci: u8,
    /// Printed colour as 8-bit RGB.
    pub color: [u8; 3],
    /// Printed lineweight in mm. 0.0 means "use entity lineweight".
    pub lineweight_mm: f64,
    /// Screening 0..=100 (% of full colour intensity).
    pub screening: u8,
    /// Optional plotted linetype name (None = use entity linetype).
    pub linetype: Option<String>,
    /// If true, ignore entity linetype and use `linetype` from the
    /// table.
    pub override_linetype: bool,
}

impl PlotStyle {
    pub fn black(aci: u8, lineweight_mm: f64) -> Self {
        Self {
            aci,
            color: [0, 0, 0],
            lineweight_mm,
            screening: 100,
            linetype: None,
            override_linetype: false,
        }
    }
}

/// CTB-style plot style table — flat, indexed by ACI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlotStyleTable {
    pub name: String,
    /// Ordered by ACI so iteration is deterministic.
    pub styles: BTreeMap<u8, PlotStyle>,
    pub apply_to_layout: bool,
}

impl PlotStyleTable {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            styles: BTreeMap::new(),
            apply_to_layout: true,
        }
    }

    pub fn insert(&mut self, style: PlotStyle) {
        self.styles.insert(style.aci, style);
    }

    pub fn get(&self, aci: u8) -> Option<&PlotStyle> {
        self.styles.get(&aci)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// "Monochrome" preset — ACI 1..=255 all map to black with the
    /// default 0.25 mm lineweight.
    pub fn monochrome() -> Self {
        let mut t = Self::new("monochrome");
        for aci in 1u8..=255 {
            t.insert(PlotStyle::black(aci, 0.25));
        }
        t
    }

    /// "Color" preset — passes through ACI to a standard 7-colour
    /// AutoCAD palette and uses 0.25 mm lineweight for everything.
    pub fn color_preset() -> Self {
        let mut t = Self::new("color");
        // Standard ACI 1..=7 colours.
        let palette: [(u8, [u8; 3]); 7] = [
            (1, [255, 0, 0]),   // red
            (2, [255, 255, 0]), // yellow
            (3, [0, 255, 0]),   // green
            (4, [0, 255, 255]), // cyan
            (5, [0, 0, 255]),   // blue
            (6, [255, 0, 255]), // magenta
            (7, [0, 0, 0]),     // white→black on paper
        ];
        for (aci, color) in palette {
            t.insert(PlotStyle {
                aci,
                color,
                lineweight_mm: 0.25,
                screening: 100,
                linetype: None,
                override_linetype: false,
            });
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_has_255_entries() {
        let t = PlotStyleTable::monochrome();
        assert_eq!(t.len(), 255);
        // Spot-check.
        let s = t.get(1).unwrap();
        assert_eq!(s.color, [0, 0, 0]);
        assert!((s.lineweight_mm - 0.25).abs() < 1e-9);
    }

    #[test]
    fn color_preset_has_seven_standard_entries() {
        let t = PlotStyleTable::color_preset();
        assert_eq!(t.len(), 7);
        assert_eq!(t.get(1).unwrap().color, [255, 0, 0]);
        assert_eq!(t.get(7).unwrap().color, [0, 0, 0]);
    }

    #[test]
    fn iteration_is_aci_sorted() {
        let mut t = PlotStyleTable::new("test");
        t.insert(PlotStyle::black(5, 0.13));
        t.insert(PlotStyle::black(1, 0.13));
        t.insert(PlotStyle::black(3, 0.13));
        let acis: Vec<u8> = t.styles.keys().copied().collect();
        assert_eq!(acis, vec![1, 3, 5]);
    }
}
