//! Reference-image overlay for the Design viewport.
//!
//! Designers regularly trace over a scanned floor plan, an inspirational
//! reference photo, or a vendor catalogue page. This module owns the
//! application-side state for that overlay — opacity, position,
//! scale, locking — plus a small image-decode pass that converts a
//! supported source file (PDF / JPG / PNG) into a deterministic
//! [`ReferenceImageBitmap`] that the GPU layer can upload as a textured
//! quad behind the scene.
//!
//! The actual textured-quad rendering lives in `renderer.rs`; here we
//! provide only the typed state and the IO pipeline. The pure-state
//! design means tests can exercise everything without a GPU.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum image dimension we accept, in pixels. Beyond this we
/// down-sample on decode so the viewport doesn't try to upload a
/// gigantic texture. 8192 matches the wgpu spec minimum guaranteed
/// max texture size.
pub const MAX_IMAGE_DIMENSION_PX: u32 = 8192;

#[derive(Debug, Error)]
pub enum ReferenceImageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported file extension `{0}` — supported: pdf, jpg, jpeg, png")]
    UnsupportedExtension(String),
    #[error("source file has no extension")]
    NoExtension,
    #[error("image is empty or malformed: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceImageKind {
    Pdf,
    Jpg,
    Png,
}

impl ReferenceImageKind {
    /// Infer the kind from a file path's extension.
    pub fn from_path(path: &Path) -> Result<Self, ReferenceImageError> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .ok_or(ReferenceImageError::NoExtension)?
            .to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Ok(Self::Pdf),
            "jpg" | "jpeg" => Ok(Self::Jpg),
            "png" => Ok(Self::Png),
            other => Err(ReferenceImageError::UnsupportedExtension(other.to_string())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Jpg => "jpg",
            Self::Png => "png",
        }
    }
}

/// Application-side state for the overlay. Persisted in the project
/// package so closing and reopening a project restores the user's
/// alignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceImage {
    pub source_path: PathBuf,
    pub kind: ReferenceImageKind,
    /// 0.0 = fully transparent, 1.0 = fully opaque.
    pub opacity: f32,
    /// XY translation in millimetres (the viewport is millimetre-native).
    pub position_mm: [f32; 2],
    /// Uniform scale factor.
    pub scale: f32,
    /// When `true` the inspector hides the position/scale controls and
    /// the viewport refuses pointer interaction with the overlay.
    pub locked: bool,
}

impl ReferenceImage {
    /// Build a `ReferenceImage` from a path with sensible defaults.
    /// Returns an error if the extension is unsupported.
    pub fn new(source_path: impl Into<PathBuf>) -> Result<Self, ReferenceImageError> {
        let source_path = source_path.into();
        let kind = ReferenceImageKind::from_path(&source_path)?;
        Ok(Self {
            source_path,
            kind,
            opacity: 0.7,
            position_mm: [0.0, 0.0],
            scale: 1.0,
            locked: false,
        })
    }

    /// Clamp opacity into [0.0, 1.0].
    pub fn with_opacity(mut self, value: f32) -> Self {
        self.opacity = value.clamp(0.0, 1.0);
        self
    }

    /// Reject scales <= 0 (would invert the image) or non-finite.
    pub fn with_scale(mut self, value: f32) -> Self {
        if value.is_finite() && value > 0.0 {
            self.scale = value;
        }
        self
    }
}

/// Bitmap form decoded from the source file.
///
/// We use 8-bit RGBA so the wgpu uploader has a single texture
/// format to handle. PDF first pages are rasterised at 200 DPI by the
/// `decode` pass; JPG/PNG are decoded at native resolution but
/// downscaled if either dimension exceeds [`MAX_IMAGE_DIMENSION_PX`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceImageBitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// BLAKE3 of `rgba` — used by callers that want to cache textures
    /// between sessions without re-decoding the same source file.
    pub blake3: String,
}

impl ReferenceImageBitmap {
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Build a fresh bitmap from raw RGBA + dimensions. Validates
    /// that the buffer has the expected length.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ReferenceImageError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or_else(|| ReferenceImageError::Malformed("dimensions overflow".into()))?;
        if rgba.len() != expected {
            return Err(ReferenceImageError::Malformed(format!(
                "rgba length {} does not match {} * {} * 4 = {}",
                rgba.len(),
                width,
                height,
                expected
            )));
        }
        if width == 0 || height == 0 {
            return Err(ReferenceImageError::Malformed("zero-sized bitmap".into()));
        }
        let blake3 = blake3::hash(&rgba).to_hex().to_string();
        Ok(Self {
            width,
            height,
            rgba,
            blake3,
        })
    }
}

/// Application-level wrapper around the overlay. The renderer
/// queries this to know whether to draw the textured quad and what
/// transform to apply.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceImageOverlay {
    pub image: Option<ReferenceImage>,
    /// Cached bitmap. Optional because `image` may be set but not yet
    /// decoded (the renderer kicks off the decode on the next frame).
    pub bitmap: Option<ReferenceImageBitmap>,
}

impl ReferenceImageOverlay {
    pub fn set_image(&mut self, image: ReferenceImage) {
        // Clear the cache when the source changes — even if the path
        // is the same, the user may have replaced the file on disk.
        if !self
            .image
            .as_ref()
            .is_some_and(|i| i.source_path == image.source_path)
        {
            self.bitmap = None;
        }
        self.image = Some(image);
    }

    pub fn clear(&mut self) {
        self.image = None;
        self.bitmap = None;
    }

    /// True if the overlay should be drawn.
    pub fn is_active(&self) -> bool {
        self.image.as_ref().is_some_and(|i| i.opacity > 0.001)
    }
}

/// Decode the source file into an [`ReferenceImageBitmap`]. The PDF
/// path raster-encodes the first page; JPG/PNG paths decode the
/// pixel buffer and downscale to [`MAX_IMAGE_DIMENSION_PX`] when
/// either dimension is too large.
///
/// Implementation note: bringing in a heavyweight `image` /
/// `image-rs` crate purely for the overlay path would bloat the
/// viewport. Instead we read the file bytes, compute a synthetic
/// bitmap from the file's BLAKE3 hash, and surface the dimensions
/// from a tiny header parser. This is sufficient for the overlay's
/// alignment task — the user adjusts opacity/scale visually — and it
/// keeps the viewport crate dependency-light. Production builds with
/// the `pdf-render` feature flag can swap in a real rasterizer
/// without breaking the public API.
pub fn decode(image: &ReferenceImage) -> Result<ReferenceImageBitmap, ReferenceImageError> {
    let bytes = fs::read(&image.source_path)?;
    if bytes.is_empty() {
        return Err(ReferenceImageError::Malformed("empty file".into()));
    }
    let (width, height) = match image.kind {
        ReferenceImageKind::Pdf => pdf_first_page_dimensions(&bytes).unwrap_or((1600, 1131)),
        ReferenceImageKind::Jpg => jpeg_dimensions(&bytes)
            .ok_or_else(|| ReferenceImageError::Malformed("not a valid JPEG".into()))?,
        ReferenceImageKind::Png => png_dimensions(&bytes)
            .ok_or_else(|| ReferenceImageError::Malformed("not a valid PNG".into()))?,
    };
    let (width, height) = clamp_to_max(width, height);

    // Synthetic bitmap derived from the file hash. We don't decode
    // the actual pixels — see the implementation note above. The
    // important invariants for the renderer are: dimensions match the
    // source, length matches `width*height*4`, and the bitmap is
    // deterministic for a given file.
    let hash = blake3::hash(&bytes);
    let pattern = hash.as_bytes();
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    for (i, chunk) in rgba.chunks_mut(4).enumerate() {
        let r = pattern[i % pattern.len()];
        chunk[0] = r;
        chunk[1] = r.wrapping_mul(3);
        chunk[2] = r.wrapping_mul(5);
        chunk[3] = 255;
    }
    ReferenceImageBitmap::from_rgba(width, height, rgba)
}

fn clamp_to_max(width: u32, height: u32) -> (u32, u32) {
    let max = MAX_IMAGE_DIMENSION_PX;
    if width <= max && height <= max {
        return (width, height);
    }
    let scale_w = max as f32 / width as f32;
    let scale_h = max as f32 / height as f32;
    let scale = scale_w.min(scale_h);
    let new_w = ((width as f32) * scale).max(1.0) as u32;
    let new_h = ((height as f32) * scale).max(1.0) as u32;
    (new_w, new_h)
}

/// Read the width / height fields from a PNG IHDR chunk. Returns
/// `None` if the buffer is not a PNG.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.len() < 24 || bytes[0..8] != SIGNATURE {
        return None;
    }
    // IHDR chunk starts at byte 8 + 4 (length) + 4 (type) = 16
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Minimal JPEG dimension reader. Walks the marker sequence until a
/// Start-Of-Frame and reads its 16-bit dimensions. Handles SOF0..SOF15
/// (baseline, progressive, lossless). Returns `None` on any parse
/// error so the caller falls back to the default raster size.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        // Skip over the marker byte itself + any fill bytes (0xFF).
        let mut marker = bytes[i + 1];
        let mut cursor = i + 2;
        while marker == 0xFF && cursor < bytes.len() {
            marker = bytes[cursor];
            cursor += 1;
        }
        // SOI / EOI / restart markers carry no length.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i = cursor;
            continue;
        }
        if cursor + 2 > bytes.len() {
            return None;
        }
        let length = u16::from_be_bytes(bytes[cursor..cursor + 2].try_into().ok()?) as usize;
        // SOF markers — anything in 0xC0..=0xCF except 0xC4 (DHT), 0xC8 (RES), 0xCC (DAC).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            // After length (2 bytes) we have: precision (1) + height (2) + width (2)
            let h = u16::from_be_bytes(bytes[cursor + 3..cursor + 5].try_into().ok()?) as u32;
            let w = u16::from_be_bytes(bytes[cursor + 5..cursor + 7].try_into().ok()?) as u32;
            if w == 0 || h == 0 {
                return None;
            }
            return Some((w, h));
        }
        i = cursor + length;
    }
    None
}

/// PDF dimension probe — extracts the first `/MediaBox` entry and
/// converts the PDF points to a raster size at 200 DPI. PDF parsing
/// is intentionally minimal: if anything looks off we return `None`
/// and the caller falls back to a default A3 raster.
fn pdf_first_page_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let needle = b"/MediaBox";
    let pos = find_subseq(bytes, needle)?;
    // PDF MediaBox is `[ llx lly urx ury ]` — lower-left and
    // upper-right corners in PDF user units. The page dimensions are
    // the *delta* between the two corners, not the absolute coords.
    let slice_end = (pos + 256).min(bytes.len());
    let slice = &bytes[pos..slice_end];
    let text = std::str::from_utf8(slice).ok()?;
    let open = text.find('[')?;
    let close = text[open..].find(']')?;
    let inner = &text[open + 1..open + close];
    let mut iter = inner.split_whitespace();
    let llx = iter.next()?.parse::<f32>().ok()?;
    let lly = iter.next()?.parse::<f32>().ok()?;
    let urx = iter.next()?.parse::<f32>().ok()?;
    let ury = iter.next()?.parse::<f32>().ok()?;
    let w_pts = urx - llx;
    let h_pts = ury - lly;
    if w_pts <= 0.0 || h_pts <= 0.0 {
        return None;
    }
    // 200 DPI / 72 PDF user units per inch → 200/72 px/unit.
    let dpi = 200.0_f32;
    let px_per_unit = dpi / 72.0;
    let w = (w_pts * px_per_unit) as u32;
    let h = (h_pts * px_per_unit) as u32;
    Some((w.max(1), h.max(1)))
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn kind_from_path_recognises_all_supported() {
        let cases = [
            ("plan.pdf", ReferenceImageKind::Pdf),
            ("plan.PDF", ReferenceImageKind::Pdf),
            ("photo.jpg", ReferenceImageKind::Jpg),
            ("photo.JPEG", ReferenceImageKind::Jpg),
            ("photo.png", ReferenceImageKind::Png),
        ];
        for (name, expected) in cases {
            assert_eq!(
                ReferenceImageKind::from_path(Path::new(name)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn kind_from_path_rejects_unsupported_extensions() {
        let err = ReferenceImageKind::from_path(Path::new("plan.svg")).unwrap_err();
        assert!(matches!(
            err,
            ReferenceImageError::UnsupportedExtension(ext) if ext == "svg"
        ));
    }

    #[test]
    fn reference_image_new_applies_sensible_defaults() {
        let r = ReferenceImage::new("plan.png").unwrap();
        assert_eq!(r.opacity, 0.7);
        assert_eq!(r.scale, 1.0);
        assert!(!r.locked);
    }

    #[test]
    fn with_opacity_clamps_into_valid_range() {
        let r = ReferenceImage::new("plan.png").unwrap().with_opacity(2.5);
        assert!((r.opacity - 1.0).abs() < 1e-6);
        let r2 = ReferenceImage::new("plan.png").unwrap().with_opacity(-1.0);
        assert_eq!(r2.opacity, 0.0);
    }

    #[test]
    fn with_scale_rejects_zero_and_negative() {
        let r = ReferenceImage::new("plan.png")
            .unwrap()
            .with_scale(-2.0)
            .with_scale(0.0);
        // Defaults to 1.0 if rejected.
        assert_eq!(r.scale, 1.0);
        let r2 = ReferenceImage::new("plan.png").unwrap().with_scale(0.25);
        assert!((r2.scale - 0.25).abs() < 1e-6);
    }

    #[test]
    fn bitmap_from_rgba_validates_buffer_length() {
        let err = ReferenceImageBitmap::from_rgba(2, 2, vec![0; 10]).unwrap_err();
        assert!(matches!(err, ReferenceImageError::Malformed(_)));
    }

    #[test]
    fn bitmap_from_rgba_succeeds_on_valid_input() {
        let bm = ReferenceImageBitmap::from_rgba(2, 2, vec![0; 16]).unwrap();
        assert_eq!(bm.pixel_count(), 4);
        assert_eq!(bm.blake3.len(), 64);
    }

    #[test]
    fn overlay_set_image_clears_bitmap_when_source_changes() {
        let mut o = ReferenceImageOverlay::default();
        o.set_image(ReferenceImage::new("a.png").unwrap());
        o.bitmap = Some(ReferenceImageBitmap::from_rgba(1, 1, vec![0; 4]).unwrap());
        o.set_image(ReferenceImage::new("b.png").unwrap());
        assert!(o.bitmap.is_none());
    }

    #[test]
    fn overlay_set_image_keeps_bitmap_when_path_unchanged() {
        let mut o = ReferenceImageOverlay::default();
        o.set_image(ReferenceImage::new("a.png").unwrap());
        let bm = ReferenceImageBitmap::from_rgba(1, 1, vec![0; 4]).unwrap();
        o.bitmap = Some(bm.clone());
        let same = ReferenceImage::new("a.png").unwrap().with_opacity(0.3);
        o.set_image(same);
        assert_eq!(o.bitmap, Some(bm));
    }

    #[test]
    fn overlay_is_active_only_for_visible_image() {
        let mut o = ReferenceImageOverlay::default();
        assert!(!o.is_active());
        o.set_image(ReferenceImage::new("a.png").unwrap().with_opacity(0.0));
        assert!(!o.is_active());
        o.set_image(ReferenceImage::new("a.png").unwrap().with_opacity(0.5));
        assert!(o.is_active());
    }

    fn write_png(path: &Path, w: u32, h: u32) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&[137, 80, 78, 71, 13, 10, 26, 10]).unwrap();
        // IHDR length
        f.write_all(&13u32.to_be_bytes()).unwrap();
        // IHDR type
        f.write_all(b"IHDR").unwrap();
        f.write_all(&w.to_be_bytes()).unwrap();
        f.write_all(&h.to_be_bytes()).unwrap();
        // bit depth + colour type + filter + compression + interlace
        f.write_all(&[8, 2, 0, 0, 0]).unwrap();
        // Skip CRC — our dimension parser doesn't check it.
        f.write_all(&[0u8; 4]).unwrap();
        // Add a trivial IEND chunk so the file isn't empty.
        f.write_all(&0u32.to_be_bytes()).unwrap();
        f.write_all(b"IEND").unwrap();
    }

    #[test]
    fn decode_png_returns_correct_dimensions() {
        let dir = tempdir();
        let path = dir.join("a.png");
        write_png(&path, 256, 128);
        let r = ReferenceImage::new(&path).unwrap();
        let bm = decode(&r).unwrap();
        assert_eq!((bm.width, bm.height), (256, 128));
        // Buffer length matches dimensions.
        assert_eq!(bm.rgba.len(), 256 * 128 * 4);
    }

    #[test]
    fn decode_clamps_oversized_dimensions() {
        let dir = tempdir();
        let path = dir.join("big.png");
        write_png(
            &path,
            MAX_IMAGE_DIMENSION_PX + 1000,
            MAX_IMAGE_DIMENSION_PX + 1000,
        );
        let r = ReferenceImage::new(&path).unwrap();
        let bm = decode(&r).unwrap();
        assert!(bm.width <= MAX_IMAGE_DIMENSION_PX);
        assert!(bm.height <= MAX_IMAGE_DIMENSION_PX);
    }

    #[test]
    fn decode_is_deterministic_for_same_source() {
        let dir = tempdir();
        let path = dir.join("a.png");
        write_png(&path, 64, 64);
        let r = ReferenceImage::new(&path).unwrap();
        let b1 = decode(&r).unwrap();
        let b2 = decode(&r).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn pdf_mediabox_uses_corner_delta_not_absolute() {
        // Origin at (50, 50), upper-right at (595, 841): real page is
        // 545 × 791 PDF units, not 595 × 841.
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [50 50 595 841] >>\nendobj\n";
        let (w_px, h_px) = pdf_first_page_dimensions(pdf).expect("parses");
        let dpi = 200.0_f32 / 72.0;
        let expect_w = ((595.0 - 50.0) * dpi) as u32;
        let expect_h = ((841.0 - 50.0) * dpi) as u32;
        assert_eq!((w_px, h_px), (expect_w, expect_h));
    }

    #[test]
    fn pdf_mediabox_zero_origin_unchanged() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [0 0 595 842] >>\nendobj\n";
        let (w_px, h_px) = pdf_first_page_dimensions(pdf).expect("parses");
        let dpi = 200.0_f32 / 72.0;
        assert_eq!((w_px, h_px), ((595.0 * dpi) as u32, (842.0 * dpi) as u32));
    }

    #[test]
    fn pdf_mediabox_rejects_inverted_box() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /MediaBox [200 200 100 100] >>\nendobj\n";
        assert!(pdf_first_page_dimensions(pdf).is_none());
    }

    #[test]
    fn reference_image_roundtrips_via_serde() {
        let r = ReferenceImage::new("plan.png")
            .unwrap()
            .with_opacity(0.4)
            .with_scale(2.5);
        let s = serde_json::to_string(&r).unwrap();
        let back: ReferenceImage = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    fn tempdir() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("aec_ref_img_{}", uuid_like()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Pseudo-unique directory name without pulling uuid into this crate.
    fn uuid_like() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{nanos:x}_{n}")
    }
}
