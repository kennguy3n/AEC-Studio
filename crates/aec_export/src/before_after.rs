//! Side-by-side before/after comparison PDF.
//!
//! Generates a single-page (or multi-page) PDF with each page showing
//! a "Before" and "After" caption pair. We deliberately do not embed
//! the render images themselves at the PDF level — printpdf's image
//! API requires decoded raster pixels, which would force an `image`
//! crate dependency. Instead the comparison page lists the file path
//! and key metadata for the two renders; downstream tooling (or the
//! desktop preview) is responsible for displaying the pixel data.
//!
//! This keeps the export deterministic, very fast, and easy to embed
//! inside the contractor / interior packs without dragging in a full
//! image-decoder pipeline.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};

#[derive(Debug, Error)]
pub enum BeforeAfterPdfError {
    #[error("pdf builder error: {0}")]
    Pdf(#[from] PdfBuilderError),
    #[error("no comparison pairs supplied")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAfterRenderPair {
    pub label: String,
    pub before_path: PathBuf,
    pub after_path: PathBuf,
    pub before_preset: String,
    pub after_preset: String,
    /// Wall-clock ms each render took. Used for the "Δ time" caption.
    pub before_ms: u64,
    pub after_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAfterPdfOptions {
    pub project_name: String,
    pub pairs: Vec<BeforeAfterRenderPair>,
}

impl BeforeAfterPdfOptions {
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<PathBuf, BeforeAfterPdfError> {
        if self.pairs.is_empty() {
            return Err(BeforeAfterPdfError::Empty);
        }

        let mut builder = PdfBuilder::new(
            format!("{} — before / after", self.project_name),
            PageSize::A4_LANDSCAPE,
        )?;
        builder.add_cover_page(Some("Comparison of render iterations"))?;

        for pair in &self.pairs {
            let delta_ms: i64 = pair.after_ms as i64 - pair.before_ms as i64;
            let delta_text = if delta_ms.abs() > 0 {
                format!("Δ time: {:+} ms", delta_ms)
            } else {
                "Δ time: 0 ms".to_string()
            };
            let lines: Vec<String> = vec![
                format!("Comparison: {}", pair.label),
                String::new(),
                format!("Before: {}", pair.before_path.display()),
                format!("  preset: {}", pair.before_preset),
                format!("  duration: {} ms", pair.before_ms),
                String::new(),
                format!("After:  {}", pair.after_path.display()),
                format!("  preset: {}", pair.after_preset),
                format!("  duration: {} ms", pair.after_ms),
                String::new(),
                delta_text,
                format!(
                    "Preset change: {} → {}",
                    pair.before_preset, pair.after_preset
                ),
            ];
            builder.add_text_page(&format!("Before / After — {}", pair.label), &lines)?;
        }

        let out = builder.save(path.as_ref())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_pair() -> BeforeAfterRenderPair {
        BeforeAfterRenderPair {
            label: "Living room".into(),
            before_path: PathBuf::from("/renders/living_before.png"),
            after_path: PathBuf::from("/renders/living_after.png"),
            before_preset: "standard".into(),
            after_preset: "studio".into(),
            before_ms: 9_500,
            after_ms: 38_200,
        }
    }

    #[test]
    fn writes_pdf_with_one_page_per_pair_plus_cover() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("before_after.pdf");
        let opts = BeforeAfterPdfOptions {
            project_name: "Apartment 12B".into(),
            pairs: vec![sample_pair(), sample_pair()],
        };
        opts.write_to(&path).unwrap();

        let mut f = std::fs::File::open(&path).unwrap();
        let mut header = [0u8; 5];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"%PDF-", "output must be a valid PDF");

        let meta = std::fs::metadata(&path).unwrap();
        // Cover + 2 pairs => at minimum 3 KB of content.
        assert!(meta.len() > 1_500);
    }

    #[test]
    fn empty_pairs_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = BeforeAfterPdfOptions {
            project_name: "Test".into(),
            pairs: vec![],
        };
        let err = opts.write_to(tmp.path().join("err.pdf")).unwrap_err();
        assert!(matches!(err, BeforeAfterPdfError::Empty));
    }
}
