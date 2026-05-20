//! Sheet set — an ordered list of sheets used for batch plotting.

use serde::{Deserialize, Serialize};

use crate::sheets::sheet::Sheet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlotTarget {
    Pdf,
    Svg,
    PdfMultiPage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintConfiguration {
    pub target: PlotTarget,
    pub include_unprintable_layers: bool,
    pub apply_plot_style: bool,
    pub plot_style_name: Option<String>,
    pub copies: u32,
}

impl Default for PrintConfiguration {
    fn default() -> Self {
        Self {
            target: PlotTarget::PdfMultiPage,
            include_unprintable_layers: false,
            apply_plot_style: true,
            plot_style_name: None,
            copies: 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetSet {
    pub name: String,
    pub sheets: Vec<Sheet>,
    pub print_config: PrintConfiguration,
}

impl SheetSet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sheets: Vec::new(),
            print_config: PrintConfiguration::default(),
        }
    }

    pub fn add(&mut self, sheet: Sheet) {
        self.sheets.push(sheet);
    }

    pub fn move_sheet(&mut self, from: usize, to: usize) -> bool {
        if from >= self.sheets.len() || to >= self.sheets.len() {
            return false;
        }
        let s = self.sheets.remove(from);
        self.sheets.insert(to, s);
        true
    }

    pub fn len(&self) -> usize {
        self.sheets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sheets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheets::sheet::PaperSize;

    #[test]
    fn move_sheet_reorders() {
        let mut s = SheetSet::new("Plot");
        s.add(Sheet::new("S1", PaperSize::IsoA3));
        s.add(Sheet::new("S2", PaperSize::IsoA3));
        s.add(Sheet::new("S3", PaperSize::IsoA3));
        assert!(s.move_sheet(0, 2));
        assert_eq!(s.sheets[0].name, "S2");
        assert_eq!(s.sheets[2].name, "S1");
    }
}
