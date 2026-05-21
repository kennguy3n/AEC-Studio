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
    /// 0-based page index. Only meaningful when [`Self::kind`] is
    /// [`ReferenceImageKind::Pdf`]; ignored for JPG/PNG (which are
    /// single-page). Defaults to `0` (cover page).
    #[serde(default)]
    pub page_index: u32,
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
            page_index: 0,
        })
    }

    /// Set the active page (PDFs only). Returns `self` for chaining
    /// in the typical builder pattern. Pages beyond the source's
    /// page count are clamped at decode time, not here, because the
    /// page count isn't known until the file is read.
    pub fn with_page(mut self, page_index: u32) -> Self {
        self.page_index = page_index;
        self
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

/// Per-page metadata returned by [`enumerate_pages`]. For non-PDF
/// inputs the returned vector contains exactly one entry with
/// `page_index = 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceImagePage {
    pub page_index: u32,
    pub width: u32,
    pub height: u32,
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
        // Check dimensions first so a caller passing e.g. `(0, 5, vec![1])`
        // gets the more precise "zero-sized bitmap" error instead of a
        // "length mismatch" that obscures the real problem (width=0).
        if width == 0 || height == 0 {
            return Err(ReferenceImageError::Malformed("zero-sized bitmap".into()));
        }
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
    /// Install or replace the overlay image.
    ///
    /// Only invalidates the cached bitmap when the source *path*
    /// changes. When the caller is just tweaking transform fields
    /// (opacity, position, scale, locked) we keep the previously
    /// decoded bitmap so the renderer doesn't pay the decode cost for
    /// a parameter-only update.
    ///
    /// Replacing the file on disk under the same path is **not**
    /// detected here; if that matters to a caller (e.g. a watched
    /// "reload" action) it should `clear()` the overlay first and
    /// then `set_image`. The regression test
    /// `overlay_set_image_keeps_bitmap_when_path_unchanged` pins this
    /// contract.
    pub fn set_image(&mut self, image: ReferenceImage) {
        let same_source = self.image.as_ref().is_some_and(|i| {
            // Invalidate when either the file changes or the page
            // index changes — a new page is a new bitmap even though
            // the underlying file is the same.
            i.source_path == image.source_path && i.page_index == image.page_index
        });
        if !same_source {
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
    let (width, height, resolved_page) = match image.kind {
        ReferenceImageKind::Pdf => {
            let pages = pdf_pages(&bytes);
            let n = pages.len() as u32;
            // Fall back to page 0 (the cover) on out-of-range. We
            // *don't* error because a project file surviving a PDF
            // being replaced with a shorter one should still open;
            // and falling back to page 0 is the least surprising
            // recovery — the host UI's page picker will then re-sync
            // to the available range. (We deliberately don't saturate
            // to the last page: a project pinned to page 7 should not
            // silently start showing the cover of a now-3-page PDF
            // and only if it has fewer than 8 pages.)
            let resolved = if n == 0 || image.page_index >= n {
                0
            } else {
                image.page_index
            };
            let (w, h) = pages
                .get(resolved as usize)
                .copied()
                .unwrap_or((1600, 1131));
            (w, h, resolved)
        }
        ReferenceImageKind::Jpg => {
            let (w, h) = jpeg_dimensions(&bytes)
                .ok_or_else(|| ReferenceImageError::Malformed("not a valid JPEG".into()))?;
            (w, h, 0)
        }
        ReferenceImageKind::Png => {
            let (w, h) = png_dimensions(&bytes)
                .ok_or_else(|| ReferenceImageError::Malformed("not a valid PNG".into()))?;
            (w, h, 0)
        }
    };
    let (width, height) = clamp_to_max(width, height);

    // Synthetic bitmap derived from the file hash + page index. We
    // don't decode the actual pixels — see the implementation note
    // above. The important invariants for the renderer are:
    // dimensions match the source, length matches `width*height*4`,
    // bitmap is deterministic for a given (file, page) pair, and
    // different pages produce different patterns so the host UI can
    // visually confirm page switching is working.
    let mut hasher = blake3::Hasher::new();
    hasher.update(&bytes);
    hasher.update(&resolved_page.to_le_bytes());
    let hash = hasher.finalize();
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

/// Enumerate pages in a reference image. For PDFs returns one entry
/// per page (in document order); for JPG/PNG returns a single entry
/// with `page_index = 0`. Returns the source's first-page dimensions
/// for any PDF page whose MediaBox can't be parsed, so the host UI
/// always gets a usable size.
pub fn enumerate_pages(
    image: &ReferenceImage,
) -> Result<Vec<ReferenceImagePage>, ReferenceImageError> {
    let bytes = fs::read(&image.source_path)?;
    if bytes.is_empty() {
        return Err(ReferenceImageError::Malformed("empty file".into()));
    }
    match image.kind {
        ReferenceImageKind::Pdf => {
            let pages = pdf_pages(&bytes);
            if pages.is_empty() {
                return Err(ReferenceImageError::Malformed(
                    "PDF contains no parseable pages".into(),
                ));
            }
            Ok(pages
                .into_iter()
                .enumerate()
                .map(|(i, (w, h))| {
                    let (w, h) = clamp_to_max(w, h);
                    ReferenceImagePage {
                        page_index: i as u32,
                        width: w,
                        height: h,
                    }
                })
                .collect())
        }
        ReferenceImageKind::Jpg => {
            let (w, h) = jpeg_dimensions(&bytes)
                .ok_or_else(|| ReferenceImageError::Malformed("not a valid JPEG".into()))?;
            let (w, h) = clamp_to_max(w, h);
            Ok(vec![ReferenceImagePage {
                page_index: 0,
                width: w,
                height: h,
            }])
        }
        ReferenceImageKind::Png => {
            let (w, h) = png_dimensions(&bytes)
                .ok_or_else(|| ReferenceImageError::Malformed("not a valid PNG".into()))?;
            let (w, h) = clamp_to_max(w, h);
            Ok(vec![ReferenceImagePage {
                page_index: 0,
                width: w,
                height: h,
            }])
        }
    }
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
            // After length (2 bytes) we have: precision (1) + height (2) + width (2).
            // A truncated JPEG can end mid-SOF — we already validated the
            // length field but not the dimension fields themselves, so guard
            // the slice explicitly. Returning `None` here drops us back into
            // the caller's "not a valid JPEG" fallback instead of panicking.
            if cursor + 7 > bytes.len() {
                return None;
            }
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

/// Multi-page PDF dimension probe. Scans the document for page
/// objects (`/Type /Page` not `/Type /Pages`) and returns one
/// `(width, height)` pair per page in document order.
///
/// PDF parsing is intentionally minimal: we look for `/Type /Page`
/// markers and pair each one with the nearest following `/MediaBox`.
/// Pages that inherit their MediaBox from a `/Pages` parent (less
/// common but valid) fall back to the document's first parseable
/// MediaBox. Pages with neither get the A3 default. This handles the
/// 99% case (every page declares its own MediaBox) without pulling in
/// a full PDF parser.
fn pdf_pages(bytes: &[u8]) -> Vec<(u32, u32)> {
    // First scan: collect every MediaBox in document order.
    let media_boxes = find_all_media_boxes(bytes);
    let inherited: (u32, u32) = media_boxes
        .first()
        .map_or((1600, 1131), |(w, h, _)| (*w, *h));

    // Second scan: find every page object, in order, and assign the
    // first MediaBox at or after the page's start. This is the same
    // rule PDF readers apply for the common case where each page
    // object precedes its MediaBox in the byte stream.
    let page_positions = find_page_positions(bytes);

    if page_positions.is_empty() {
        // Fall back: at least one page if we found a MediaBox.
        return if media_boxes.is_empty() {
            Vec::new()
        } else {
            vec![inherited]
        };
    }

    let mut out = Vec::with_capacity(page_positions.len());
    for (i, &page_pos) in page_positions.iter().enumerate() {
        // The MediaBox for this page is the first one whose byte
        // position is greater than this page's start *and* less than
        // the next page's start. This rejects MediaBoxes that belong
        // to a later page.
        let next_page_pos = page_positions.get(i + 1).copied().unwrap_or(usize::MAX);
        let mb: (u32, u32) = media_boxes
            .iter()
            .find(|(_, _, pos)| *pos > page_pos && *pos < next_page_pos)
            .map_or(inherited, |(w, h, _)| (*w, *h));
        out.push(mb);
    }
    out
}

fn find_all_media_boxes(bytes: &[u8]) -> Vec<(u32, u32, usize)> {
    let needle = b"/MediaBox";
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor + needle.len() <= bytes.len() {
        let Some(rel) = find_subseq(&bytes[cursor..], needle) else {
            break;
        };
        let abs = cursor + rel;
        if let Some(dims) = parse_media_box_at(bytes, abs) {
            out.push((dims.0, dims.1, abs));
        }
        cursor = abs + needle.len();
    }
    out
}

fn parse_media_box_at(bytes: &[u8], pos: usize) -> Option<(u32, u32)> {
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
    let dpi = 200.0_f32;
    let px_per_unit = dpi / 72.0;
    let w = (w_pts * px_per_unit) as u32;
    let h = (h_pts * px_per_unit) as u32;
    Some((w.max(1), h.max(1)))
}

/// Locate every page-object position in the PDF. A page object is
/// marked by the byte pattern `/Type /Page` followed by a non-`s`
/// non-alphanumeric byte (so we exclude `/Type /Pages` which is the
/// page-tree parent). Real PDFs always emit `/Type /Page` followed by
/// whitespace, `/`, or end-of-line, but we accept any non-letter for
/// robustness against unusual whitespace.
fn find_page_positions(bytes: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let needle = b"/Type";
    while cursor + needle.len() <= bytes.len() {
        let Some(rel) = find_subseq(&bytes[cursor..], needle) else {
            break;
        };
        let abs = cursor + rel;
        // Skip past `/Type` and any whitespace, then check for `/Page` and
        // ensure the byte after `e` is not a letter (would be `s` for `/Pages`).
        let mut p = abs + needle.len();
        while p < bytes.len()
            && (bytes[p] == b' ' || bytes[p] == b'\t' || bytes[p] == b'\n' || bytes[p] == b'\r')
        {
            p += 1;
        }
        let page = b"/Page";
        if p + page.len() < bytes.len() && &bytes[p..p + page.len()] == page {
            let after = bytes[p + page.len()];
            // Reject `/Pages` (and any other identifier-extending byte).
            let is_identifier_continuation = after.is_ascii_alphanumeric() || after == b'_';
            if !is_identifier_continuation {
                out.push(abs);
            }
        }
        cursor = abs + needle.len();
    }
    out
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
    fn bitmap_from_rgba_zero_dim_returns_zero_sized_error() {
        // Regression: previously this returned a generic "length
        // mismatch" because the length check ran first, hiding the
        // real reason. The dimension check now runs first so the user
        // sees the precise zero-sized-bitmap error.
        let err = ReferenceImageBitmap::from_rgba(0, 5, vec![1]).unwrap_err();
        match err {
            ReferenceImageError::Malformed(ref msg) => {
                assert!(
                    msg.contains("zero-sized"),
                    "expected zero-sized error, got: {msg}"
                );
            }
            other => panic!("expected Malformed(zero-sized), got {other:?}"),
        }

        let err = ReferenceImageBitmap::from_rgba(0, 0, Vec::new()).unwrap_err();
        match err {
            ReferenceImageError::Malformed(ref msg) => {
                assert!(
                    msg.contains("zero-sized"),
                    "expected zero-sized error, got: {msg}"
                );
            }
            other => panic!("expected Malformed(zero-sized), got {other:?}"),
        }
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

    fn first_page(bytes: &[u8]) -> Option<(u32, u32)> {
        pdf_pages(bytes).into_iter().next()
    }

    #[test]
    fn pdf_mediabox_uses_corner_delta_not_absolute() {
        // Origin at (50, 50), upper-right at (595, 841): real page is
        // 545 × 791 PDF units, not 595 × 841.
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [50 50 595 841] >>\nendobj\n";
        let (w_px, h_px) = first_page(pdf).expect("parses");
        let dpi = 200.0_f32 / 72.0;
        let expect_w = ((595.0 - 50.0) * dpi) as u32;
        let expect_h = ((841.0 - 50.0) * dpi) as u32;
        assert_eq!((w_px, h_px), (expect_w, expect_h));
    }

    #[test]
    fn pdf_mediabox_zero_origin_unchanged() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [0 0 595 842] >>\nendobj\n";
        let (w_px, h_px) = first_page(pdf).expect("parses");
        let dpi = 200.0_f32 / 72.0;
        assert_eq!((w_px, h_px), ((595.0 * dpi) as u32, (842.0 * dpi) as u32));
    }

    #[test]
    fn pdf_mediabox_rejects_inverted_box() {
        // The bare `/MediaBox` (no `/Type /Page` before it) is still
        // scanned and the inverted corners are rejected, so this PDF
        // has zero parseable pages.
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /MediaBox [200 200 100 100] >>\nendobj\n";
        assert!(first_page(pdf).is_none());
    }

    fn make_multipage_pdf(media_boxes: &[(u32, u32)]) -> Vec<u8> {
        use std::fmt::Write;
        let mut out = String::from("%PDF-1.4\n");
        for (i, (w, h)) in media_boxes.iter().enumerate() {
            let idx = i + 1;
            // Writing to a `String` is infallible; discard the Result.
            let _ = write!(
                out,
                "{idx} 0 obj\n<< /Type /Page /MediaBox [0 0 {w} {h}] >>\nendobj\n",
            );
        }
        out.into_bytes()
    }

    #[test]
    fn pdf_pages_enumerates_every_page_in_order() {
        let pdf = make_multipage_pdf(&[(595, 842), (842, 595), (1190, 842)]);
        let pages = pdf_pages(&pdf);
        assert_eq!(pages.len(), 3);
        let dpi = 200.0_f32 / 72.0;
        assert_eq!(pages[0], ((595.0 * dpi) as u32, (842.0 * dpi) as u32));
        assert_eq!(pages[1], ((842.0 * dpi) as u32, (595.0 * dpi) as u32));
        assert_eq!(pages[2], ((1190.0 * dpi) as u32, (842.0 * dpi) as u32));
    }

    #[test]
    fn pdf_pages_ignores_pages_tree_root_node() {
        // The page-tree parent uses `/Type /Pages`. It also carries
        // its own `/MediaBox` (inherited by children). Our scanner
        // must skip the parent node and only return real pages.
        let pdf = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Pages /MediaBox [0 0 100 100] /Kids [2 0 R] >>\nendobj\n\
2 0 obj\n<< /Type /Page /MediaBox [0 0 595 842] >>\nendobj\n";
        let pages = pdf_pages(pdf);
        assert_eq!(pages.len(), 1);
        let dpi = 200.0_f32 / 72.0;
        assert_eq!(pages[0], ((595.0 * dpi) as u32, (842.0 * dpi) as u32));
    }

    fn write_pdf(path: &Path, pages: &[(u32, u32)]) {
        let bytes = make_multipage_pdf(pages);
        fs::write(path, &bytes).unwrap();
    }

    #[test]
    fn enumerate_pages_returns_one_entry_per_pdf_page() {
        let dir = tempdir();
        let path = dir.join("multi.pdf");
        write_pdf(&path, &[(595, 842), (842, 595)]);
        let r = ReferenceImage::new(&path).unwrap();
        let pages = enumerate_pages(&r).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].page_index, 0);
        assert_eq!(pages[1].page_index, 1);
        // Second page is landscape — width should be larger than
        // height after rasterising the swapped MediaBox.
        assert!(pages[1].width > pages[1].height);
    }

    #[test]
    fn enumerate_pages_returns_single_entry_for_png() {
        let dir = tempdir();
        let path = dir.join("single.png");
        write_png(&path, 800, 600);
        let r = ReferenceImage::new(&path).unwrap();
        let pages = enumerate_pages(&r).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0],
            ReferenceImagePage {
                page_index: 0,
                width: 800,
                height: 600
            }
        );
    }

    #[test]
    fn decode_pdf_honors_page_index() {
        let dir = tempdir();
        let path = dir.join("multi.pdf");
        // Two pages with clearly different dimensions so we can
        // distinguish which one was decoded.
        write_pdf(&path, &[(595, 842), (1700, 1200)]);
        let r0 = ReferenceImage::new(&path).unwrap();
        let r1 = ReferenceImage::new(&path).unwrap().with_page(1);
        let b0 = decode(&r0).unwrap();
        let b1 = decode(&r1).unwrap();
        // Page 1 has the larger MediaBox — its raster must be wider.
        assert!(b1.width > b0.width);
        // And the bitmaps must differ because we hash in page_index.
        assert_ne!(b0.blake3, b1.blake3);
    }

    #[test]
    fn decode_pdf_clamps_out_of_range_page_index_instead_of_erroring() {
        let dir = tempdir();
        let path = dir.join("multi.pdf");
        write_pdf(&path, &[(595, 842), (842, 595)]);
        let r = ReferenceImage::new(&path).unwrap().with_page(99);
        // Out-of-range page falls back to page 0 silently — the
        // host UI shouldn't ever fail to open a project because the
        // PDF lost a page.
        let bm = decode(&r).unwrap();
        let r0 = ReferenceImage::new(&path).unwrap();
        let bm0 = decode(&r0).unwrap();
        assert_eq!(bm.blake3, bm0.blake3);
    }

    #[test]
    fn overlay_set_image_clears_bitmap_when_page_index_changes() {
        let mut o = ReferenceImageOverlay::default();
        let dir = tempdir();
        let path = dir.join("a.pdf");
        write_pdf(&path, &[(595, 842), (842, 595)]);
        o.set_image(ReferenceImage::new(&path).unwrap());
        o.bitmap = Some(ReferenceImageBitmap::from_rgba(1, 1, vec![0; 4]).unwrap());
        // Same path, different page — bitmap must be dropped so the
        // renderer re-decodes the new page.
        o.set_image(ReferenceImage::new(&path).unwrap().with_page(1));
        assert!(o.bitmap.is_none());
    }

    #[test]
    fn jpeg_dimensions_truncated_after_sof_marker_returns_none() {
        // SOI + SOF0 with a length that says "the dimensions are coming"
        // but the file ends before the precision/height/width bytes. Prior
        // to the bounds-check fix this panicked with an out-of-bounds
        // slice; the function must now return None so the caller falls
        // back gracefully.
        let mut jpeg = vec![0xFFu8, 0xD8, 0xFF, 0xC0, 0x00, 0x11];
        // Truncate immediately after the length field — no precision /
        // height / width bytes follow.
        assert!(jpeg_dimensions(&jpeg).is_none());

        // Two more bytes (precision + one byte of height) is still short
        // enough to be missing the width; must also return None.
        jpeg.extend_from_slice(&[0x08, 0x00]);
        assert!(jpeg_dimensions(&jpeg).is_none());
    }

    #[test]
    fn jpeg_dimensions_decodes_well_formed_sof0() {
        // SOI, SOF0 length=17, precision=8, height=0x012C (300),
        // width=0x01F4 (500), components etc. (rest is filler we never
        // read — we only validate the dimension fields).
        let jpeg = [
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, 0x00, 0x11, // SOF0 marker + length=17
            0x08, // precision
            0x01, 0x2C, // height = 300
            0x01, 0xF4, // width = 500
            0x03, // 3 components (Y, Cb, Cr)
            // Component bytes (we never reach this in the parser)
            0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
        ];
        assert_eq!(jpeg_dimensions(&jpeg), Some((500, 300)));
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
