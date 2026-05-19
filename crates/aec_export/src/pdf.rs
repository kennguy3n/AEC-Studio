//! Low-level PDF builder. Wraps `printpdf` and exposes the *just enough*
//! API the higher-level proposal/schedule writers need.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use printpdf::{BuiltinFont, IndirectFontRef, Mm, PdfDocument, PdfDocumentReference};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PdfBuilderError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pdf: {0}")]
    Pdf(String),
}

impl From<printpdf::Error> for PdfBuilderError {
    fn from(e: printpdf::Error) -> Self {
        Self::Pdf(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageSize {
    pub width_mm: f32,
    pub height_mm: f32,
}

impl PageSize {
    pub const A4_PORTRAIT: Self = Self {
        width_mm: 210.0,
        height_mm: 297.0,
    };
    pub const A4_LANDSCAPE: Self = Self {
        width_mm: 297.0,
        height_mm: 210.0,
    };
    pub const LETTER_PORTRAIT: Self = Self {
        width_mm: 215.9,
        height_mm: 279.4,
    };
}

pub struct PdfBuilder {
    doc: PdfDocumentReference,
    title: String,
    title_font: IndirectFontRef,
    body_font: IndirectFontRef,
    size: PageSize,
    /// Count of pages added so the consumer can sanity-check.
    page_count: u32,
}

impl PdfBuilder {
    pub fn new(title: impl Into<String>, size: PageSize) -> Result<Self, PdfBuilderError> {
        let title = title.into();
        let doc = PdfDocument::empty(&title);
        let title_font = doc.add_builtin_font(BuiltinFont::HelveticaBold)?;
        let body_font = doc.add_builtin_font(BuiltinFont::Helvetica)?;
        Ok(Self {
            doc,
            title,
            title_font,
            body_font,
            size,
            page_count: 0,
        })
    }

    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Add a cover page with a centred title and optional subtitle line.
    pub fn add_cover_page(&mut self, subtitle: Option<&str>) -> Result<(), PdfBuilderError> {
        let (page, layer) =
            self.doc
                .add_page(Mm(self.size.width_mm), Mm(self.size.height_mm), "cover");
        self.page_count += 1;
        let layer_ref = self.doc.get_page(page).get_layer(layer);
        // Title roughly centred 2/3 up the page.
        let title_y = self.size.height_mm * 0.66;
        layer_ref.use_text(
            &self.title,
            36.0,
            Mm(self.size.width_mm * 0.18),
            Mm(title_y),
            &self.title_font,
        );
        if let Some(s) = subtitle {
            layer_ref.use_text(
                s,
                14.0,
                Mm(self.size.width_mm * 0.18),
                Mm(title_y - 14.0),
                &self.body_font,
            );
        }
        Ok(())
    }

    /// Add a left-aligned text page.
    pub fn add_text_page(&mut self, title: &str, lines: &[String]) -> Result<(), PdfBuilderError> {
        let (page, layer) =
            self.doc
                .add_page(Mm(self.size.width_mm), Mm(self.size.height_mm), title);
        self.page_count += 1;
        let layer_ref = self.doc.get_page(page).get_layer(layer);
        let left = Mm(20.0_f32);
        let mut y = self.size.height_mm - 30.0;
        layer_ref.use_text(title, 22.0, left, Mm(y), &self.title_font);
        y -= 16.0;
        for line in lines {
            if y < 20.0 {
                break;
            }
            layer_ref.use_text(line, 11.0, left, Mm(y), &self.body_font);
            y -= 6.0;
        }
        Ok(())
    }

    /// Add a tabular page. Rows are clipped to fit page height; no
    /// pagination — call this multiple times if you have more than ~30 rows.
    pub fn add_table_page(
        &mut self,
        title: &str,
        headers: &[String],
        rows: &[Vec<String>],
    ) -> Result<(), PdfBuilderError> {
        let (page, layer) =
            self.doc
                .add_page(Mm(self.size.width_mm), Mm(self.size.height_mm), title);
        self.page_count += 1;
        let layer_ref = self.doc.get_page(page).get_layer(layer);
        let left = 20.0_f32;
        let col_w = (self.size.width_mm - 40.0) / headers.len().max(1) as f32;
        let mut y = self.size.height_mm - 30.0;
        layer_ref.use_text(title, 22.0, Mm(left), Mm(y), &self.title_font);
        y -= 16.0;
        for (i, header) in headers.iter().enumerate() {
            layer_ref.use_text(
                header,
                12.0,
                Mm(left + (i as f32) * col_w),
                Mm(y),
                &self.title_font,
            );
        }
        y -= 8.0;
        for row in rows {
            if y < 20.0 {
                break;
            }
            for (i, cell) in row.iter().enumerate() {
                layer_ref.use_text(
                    cell,
                    10.0,
                    Mm(left + (i as f32) * col_w),
                    Mm(y),
                    &self.body_font,
                );
            }
            y -= 6.0;
        }
        Ok(())
    }

    /// Persist the document to `path`.
    pub fn save(self, path: impl AsRef<Path>) -> Result<PathBuf, PdfBuilderError> {
        let path = path.as_ref().to_path_buf();
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        self.doc.save(&mut writer)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_three_page_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = PdfBuilder::new("Test Project", PageSize::A4_PORTRAIT).unwrap();
        b.add_cover_page(Some("Designed by AEC Studio")).unwrap();
        b.add_text_page("Overview", &["Line A".into(), "Line B".into()])
            .unwrap();
        b.add_table_page(
            "Schedule",
            &["Name".into(), "Qty".into()],
            &[vec!["Oak board".into(), "12".into()]],
        )
        .unwrap();
        assert_eq!(b.page_count(), 3);
        let path = b.save(dir.path().join("out.pdf")).unwrap();
        // Validate the saved file has the PDF magic header.
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // And weighs more than the empty doc threshold (rough sanity check).
        assert!(bytes.len() > 1024);
    }
}
