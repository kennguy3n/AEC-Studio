//! Render comparison utilities — primarily the Structural Similarity
//! Index (SSIM, Wang et al. 2004) for quantitative before/after
//! comparison of rendered images.
//!
//! The implementation follows the original paper's formula with a
//! Gaussian-weighted local window. SSIM values are in `[-1, 1]`:
//!   * `1.0` ↔ pixel-identical images,
//!   * `~0.95-0.99` ↔ visually indistinguishable (typical denoiser-vs-noisy),
//!   * `~0.7-0.9` ↔ clearly different but similar structure,
//!   * `< 0.5` ↔ structurally very different.
//!
//! Pixels are expected as linear-space RGBA (the same format used by
//! [`crate::scheduler::AccumulationBuffer::average_rgb`]). Alpha is
//! ignored. The buffer sizes and image dimensions must match; the
//! flat-byte-slice entry point returns [`CompareError::BufferSize`]
//! when the input lengths are inconsistent with `width × height × 4`,
//! while the file-decoding entry point returns
//! [`CompareError::DimensionMismatch`] when the two decoded images
//! disagree on dimensions.

use std::path::Path;

use image::GenericImageView;
use thiserror::Error;

/// Errors from [`compare_images`] / [`ssim_from_files`].
#[derive(Debug, Error)]
pub enum CompareError {
    /// One of the flat byte slices passed to [`ssim_rgba_u8`] does not
    /// match the declared `width × height × 4`. Reported separately
    /// from [`Self::DimensionMismatch`] because at this point the
    /// caller has only handed us a flat buffer — we don't know the
    /// 2D dimensions of the offending side, only that its length is
    /// wrong.
    #[error(
        "buffer size mismatch: expected {expected_bytes} bytes for {width}x{height}x4, \
         got a={a_bytes} bytes, b={b_bytes} bytes"
    )]
    BufferSize {
        width: u32,
        height: u32,
        expected_bytes: usize,
        a_bytes: usize,
        b_bytes: usize,
    },
    /// The two images compared by [`ssim_from_files`] /
    /// [`compare_images`] decoded successfully but disagree on
    /// dimensions.
    #[error("dimension mismatch: a={a_width}x{a_height}, b={b_width}x{b_height}")]
    DimensionMismatch {
        a_width: u32,
        a_height: u32,
        b_width: u32,
        b_height: u32,
    },
    #[error("image decode failed for {path}: {source}")]
    Decode {
        path: std::path::PathBuf,
        #[source]
        source: image::ImageError,
    },
}

/// Compute SSIM between two RGBA images (8-bit-per-channel inputs).
/// Each image is `width × height` pixels stored row-major as
/// `[r, g, b, a]` bytes; alpha is ignored.
///
/// The constants `k1=0.01`, `k2=0.03`, `L=255` follow Wang et al.
/// (2004) §III-B and match the standard `scikit-image.metrics.ssim`
/// implementation when called with `data_range=255` and `gaussian_weights=False`
/// (we use a uniform `11×11` window).
pub fn ssim_rgba_u8(a: &[u8], b: &[u8], width: u32, height: u32) -> Result<f32, CompareError> {
    // Both inputs are flat byte slices: we don't know their true 2D
    // dimensions if their length is wrong, so report the actual byte
    // counts rather than inventing a fictitious `b_width × 1`
    // dimension (which the previous error variant produced).
    let expected_bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if a.len() != expected_bytes || b.len() != expected_bytes {
        return Err(CompareError::BufferSize {
            width,
            height,
            expected_bytes,
            a_bytes: a.len(),
            b_bytes: b.len(),
        });
    }
    // Convert to luminance (BT.709) for a single-channel SSIM. For
    // colour, the canonical approach is per-channel SSIM averaged;
    // both have R² > 0.95 against subjective scores per Hore & Ziou
    // (2010). We use luminance for speed.
    let n = (width * height) as usize;
    let mut lum_a = vec![0.0_f32; n];
    let mut lum_b = vec![0.0_f32; n];
    for i in 0..n {
        let ra = a[i * 4] as f32;
        let ga = a[i * 4 + 1] as f32;
        let ba = a[i * 4 + 2] as f32;
        let rb = b[i * 4] as f32;
        let gb = b[i * 4 + 1] as f32;
        let bb = b[i * 4 + 2] as f32;
        lum_a[i] = 0.2126 * ra + 0.7152 * ga + 0.0722 * ba;
        lum_b[i] = 0.2126 * rb + 0.7152 * gb + 0.0722 * bb;
    }
    Ok(ssim_luminance(&lum_a, &lum_b, width, height))
}

/// SSIM for two single-channel images stored as f32 in `[0, 255]`.
///
/// Uses a uniform `11×11` window (the classic SSIM kernel). The mean
/// value is computed as the average of windowed SSIM over all pixels
/// whose window fits inside the image; boundary pixels are excluded
/// from the mean (matches scikit-image's `crop=True` mode).
pub fn ssim_luminance(a: &[f32], b: &[f32], width: u32, height: u32) -> f32 {
    let w = width as usize;
    let h = height as usize;
    const K1: f32 = 0.01;
    const K2: f32 = 0.03;
    const L: f32 = 255.0;
    const WINDOW: i32 = 11;
    let c1 = (K1 * L).powi(2);
    let c2 = (K2 * L).powi(2);
    let half = WINDOW / 2;
    if w < WINDOW as usize || h < WINDOW as usize {
        // Image too small for the window — fall back to a single
        // covariance over the whole image, which is the limit
        // behaviour of SSIM for `window == image`.
        return whole_image_ssim(a, b, c1, c2);
    }
    let win_pixels = (WINDOW * WINDOW) as f32;
    let mut acc = 0.0_f32;
    let mut count = 0_u64;
    for y in half..(h as i32 - half) {
        for x in half..(w as i32 - half) {
            let mut mu_a = 0.0;
            let mut mu_b = 0.0;
            for dy in -half..=half {
                for dx in -half..=half {
                    let idx = ((y + dy) as usize) * w + (x + dx) as usize;
                    mu_a += a[idx];
                    mu_b += b[idx];
                }
            }
            mu_a /= win_pixels;
            mu_b /= win_pixels;
            let mut var_a = 0.0;
            let mut var_b = 0.0;
            let mut cov_ab = 0.0;
            for dy in -half..=half {
                for dx in -half..=half {
                    let idx = ((y + dy) as usize) * w + (x + dx) as usize;
                    let da = a[idx] - mu_a;
                    let db = b[idx] - mu_b;
                    var_a += da * da;
                    var_b += db * db;
                    cov_ab += da * db;
                }
            }
            var_a /= win_pixels;
            var_b /= win_pixels;
            cov_ab /= win_pixels;
            let numerator = (2.0 * mu_a * mu_b + c1) * (2.0 * cov_ab + c2);
            let denominator = (mu_a * mu_a + mu_b * mu_b + c1) * (var_a + var_b + c2);
            acc += numerator / denominator;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    acc / count as f32
}

fn whole_image_ssim(a: &[f32], b: &[f32], c1: f32, c2: f32) -> f32 {
    let n = a.len() as f32;
    if n <= 0.0 {
        return 0.0;
    }
    let mu_a: f32 = a.iter().sum::<f32>() / n;
    let mu_b: f32 = b.iter().sum::<f32>() / n;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    let mut cov_ab = 0.0;
    for i in 0..a.len() {
        let da = a[i] - mu_a;
        let db = b[i] - mu_b;
        var_a += da * da;
        var_b += db * db;
        cov_ab += da * db;
    }
    var_a /= n;
    var_b /= n;
    cov_ab /= n;
    let numerator = (2.0 * mu_a * mu_b + c1) * (2.0 * cov_ab + c2);
    let denominator = (mu_a * mu_a + mu_b * mu_b + c1) * (var_a + var_b + c2);
    numerator / denominator
}

/// Load two image files from disk and compute SSIM. Convenience
/// wrapper around [`ssim_rgba_u8`] used by `Render.tsx`'s
/// before/after compare widget.
pub fn ssim_from_files(
    a_path: impl AsRef<Path>,
    b_path: impl AsRef<Path>,
) -> Result<f32, CompareError> {
    let a = image::open(a_path.as_ref()).map_err(|e| CompareError::Decode {
        path: a_path.as_ref().to_path_buf(),
        source: e,
    })?;
    let b = image::open(b_path.as_ref()).map_err(|e| CompareError::Decode {
        path: b_path.as_ref().to_path_buf(),
        source: e,
    })?;
    let (aw, ah) = a.dimensions();
    let (bw, bh) = b.dimensions();
    if aw != bw || ah != bh {
        return Err(CompareError::DimensionMismatch {
            a_width: aw,
            a_height: ah,
            b_width: bw,
            b_height: bh,
        });
    }
    let a8 = a.to_rgba8();
    let b8 = b.to_rgba8();
    ssim_rgba_u8(&a8, &b8, aw, ah)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_image(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&rgba);
        }
        v
    }

    #[test]
    fn identical_images_score_one() {
        let img = solid_image(32, 32, [128, 200, 75, 255]);
        let s = ssim_rgba_u8(&img, &img, 32, 32).expect("ssim");
        assert!((s - 1.0).abs() < 1e-3, "expected ~1.0, got {s}");
    }

    #[test]
    fn pure_noise_vs_clean_is_below_one_but_positive() {
        let w = 32;
        let h = 32;
        let clean = solid_image(w, h, [128, 128, 128, 255]);
        // Add ±25 noise.
        let mut noisy = clean.clone();
        for i in 0..(w * h) as usize {
            let n = if i % 2 == 0 { 25 } else { -25_i32 };
            for c in 0..3 {
                noisy[i * 4 + c] = (clean[i * 4 + c] as i32 + n).clamp(0, 255) as u8;
            }
        }
        let s = ssim_rgba_u8(&clean, &noisy, w, h).expect("ssim");
        assert!(s < 1.0, "noisy SSIM should be < 1.0, got {s}");
        assert!(s > 0.0, "noisy SSIM should be > 0, got {s}");
    }

    #[test]
    fn denoised_vs_noisy_scores_above_eighty_percent() {
        // Build a "clean" gradient ramp and a "noisy" version with
        // small random perturbations — the denoised image (= clean)
        // should score > 0.8 against the noisy image.
        let w = 64;
        let h = 64;
        let mut clean = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = ((x + y) * 4).min(255) as u8;
                clean.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let mut noisy = clean.clone();
        // Small ±10 noise.
        let mut rng = fastrand::Rng::with_seed(42);
        for i in 0..(w * h) as usize {
            let n: i32 = rng.i32(-10..=10);
            for c in 0..3 {
                noisy[i * 4 + c] = (clean[i * 4 + c] as i32 + n).clamp(0, 255) as u8;
            }
        }
        let s = ssim_rgba_u8(&clean, &noisy, w, h).expect("ssim");
        assert!(s > 0.8, "denoised SSIM should be > 0.8, got {s}");
        assert!(s < 1.0, "denoised SSIM should be < 1.0, got {s}");
    }

    #[test]
    fn mismatched_buffer_size_returns_buffer_size_error() {
        // `b` has 16*8*4 = 512 bytes but the caller declared an 8x8
        // image (256 bytes). The error must report both actual byte
        // counts — not fabricate a phantom `b_width × 1` dimension.
        let a = solid_image(8, 8, [0, 0, 0, 255]);
        let b = solid_image(16, 8, [0, 0, 0, 255]);
        let err = ssim_rgba_u8(&a, &b, 8, 8).unwrap_err();
        match err {
            CompareError::BufferSize {
                width,
                height,
                expected_bytes,
                a_bytes,
                b_bytes,
            } => {
                assert_eq!(width, 8);
                assert_eq!(height, 8);
                assert_eq!(expected_bytes, 8 * 8 * 4);
                assert_eq!(a_bytes, 8 * 8 * 4);
                assert_eq!(b_bytes, 16 * 8 * 4);
            }
            other => panic!("expected BufferSize, got {other:?}"),
        }
    }

    #[test]
    fn buffer_size_error_message_contains_actual_byte_counts() {
        let a = solid_image(2, 2, [0, 0, 0, 255]);
        let b = solid_image(4, 4, [0, 0, 0, 255]);
        let err = ssim_rgba_u8(&a, &b, 2, 2).unwrap_err();
        let msg = err.to_string();
        // Display impl must spell out the actual byte counts so a
        // dev reading the error knows which side was wrong-sized
        // — the previous variant invented a `b_width × 1` shape
        // that was always misleading.
        assert!(msg.contains("16 bytes"), "expected expected_bytes: {msg}");
        assert!(msg.contains("a=16 bytes"), "expected a_bytes: {msg}");
        assert!(msg.contains("b=64 bytes"), "expected b_bytes: {msg}");
    }
}
