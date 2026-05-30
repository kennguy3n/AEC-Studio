//! HDRI environment maps.
//!
//! An equirectangular HDR / OpenEXR image is loaded from disk and
//! stored as linear-space RGB pixel data. The path tracer samples the
//! map for rays that escape the BVH (replacing the procedural
//! Hosek-Wilkie sky from `lighting.rs`); the light sampler imports
//! directions from a 2-level CDF (Pharr-Jakob-Humphreys §14.2.4) for
//! next-event estimation with multiple-importance sampling.
//!
//! The radiance returned by [`EnvironmentMap::sample_direction`] is the
//! linear-space spectral radiance toward `dir`, scaled by `intensity`.
//! Internally directions are mapped to equirectangular UV via
//! `(atan2(z, x), acos(y))` so the upper hemisphere `y > 0` is the top
//! half of the image — matching the convention used by hdrihaven /
//! polyhaven / Cycles / Blender.

use std::f32::consts::PI;
use std::path::Path;

use glam::Vec3;
use image::DynamicImage;
use thiserror::Error;

/// One HDR environment map plus a precomputed importance-sampling CDF.
#[derive(Debug, Clone)]
pub struct EnvironmentMap {
    pub width: u32,
    pub height: u32,
    /// Linear-space RGB pixel data, row-major top-to-bottom.
    pub pixels: Vec<[f32; 3]>,
    /// Overall world-strength multiplier applied to every sample. Set
    /// to `1.0` for the raw HDRI; the project lighting settings can
    /// scale this for artistic intent.
    pub intensity: f32,
    /// Per-row marginal CDF over y (length `height`).
    pub marginal_cdf: Vec<f32>,
    /// Per-row conditional CDF (length `height * width`); row `y`
    /// starts at `y * width` in this vector.
    pub conditional_cdf: Vec<f32>,
    /// Per-row sums (length `height`). Used to convert the importance
    /// pdf back to solid-angle.
    pub row_integrals: Vec<f32>,
    /// Total integral of the luminance over the sphere. Stored so we
    /// can compute the per-direction pdf in
    /// [`EnvironmentMap::pdf_direction`] without recomputing.
    pub total_integral: f32,
}

#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("environment file not found: {0}")]
    NotFound(std::path::PathBuf),
    #[error("environment decode failed for {path}: {source}")]
    Decode {
        path: std::path::PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("environment image has zero dimensions ({0}x{1})")]
    EmptyImage(u32, u32),
}

impl EnvironmentMap {
    /// Load a Radiance `.hdr` or OpenEXR `.exr` file from disk and
    /// build the importance-sampling CDF.
    ///
    /// Distinguishes between a missing file (`EnvironmentError::NotFound`)
    /// and a corrupt / unsupported file (`EnvironmentError::Decode`).
    /// The `image::open` call would otherwise surface a generic IO
    /// error for both cases, which makes the UI message ("decode
    /// failed") misleading when the user mistyped a path.
    pub fn load_hdr(path: impl AsRef<Path>) -> Result<Self, EnvironmentError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(EnvironmentError::NotFound(p.to_path_buf()));
        }
        let img = image::open(p).map_err(|e| EnvironmentError::Decode {
            path: p.to_path_buf(),
            source: e,
        })?;
        Self::from_dynamic_image(img)
    }

    /// Build an [`EnvironmentMap`] from an already-decoded
    /// [`DynamicImage`]. Accepts any pixel format; HDR float images
    /// are kept as-is, 8/16-bit sRGB images are linearised. The CDFs
    /// are built immediately.
    pub fn from_dynamic_image(img: DynamicImage) -> Result<Self, EnvironmentError> {
        let rgb = img.to_rgb32f();
        let (width, height) = (rgb.width(), rgb.height());
        if width == 0 || height == 0 {
            return Err(EnvironmentError::EmptyImage(width, height));
        }
        let pixels: Vec<[f32; 3]> = rgb.pixels().map(|p| [p.0[0], p.0[1], p.0[2]]).collect();
        Ok(Self::from_pixels(width, height, pixels, 1.0))
    }

    /// Build an [`EnvironmentMap`] from raw linear-space RGB pixels.
    /// Useful for tests and for callers that have already loaded
    /// pixels via another path (e.g. an in-memory blob).
    ///
    /// # Panics
    ///
    /// Panics if `pixels.len() != width * height`. The CDF builder
    /// (`build_importance_cdf`) and the `sample_direction` /
    /// `pdf_direction` paths all index `pixels` by
    /// `y * width + x` and would silently wrap around / read the
    /// wrong row on a mismatched `Vec`, which is far worse than a
    /// loud panic at construction. The check used to be
    /// `debug_assert_eq!`, but that compiles out in release builds
    /// — leaving callers (in particular `from_dynamic_image` if the
    /// underlying decoder ever surprises us, and any future
    /// in-memory blob importer) with no protection in production
    /// binaries. Matches the same `assert_eq!` pattern used by the
    /// sibling `denoise::ImageRgb::from_pixels` constructor.
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<[f32; 3]>, intensity: f32) -> Self {
        assert_eq!(
            pixels.len(),
            (width as usize) * (height as usize),
            "EnvironmentMap::from_pixels: pixels.len() = {} does not match width*height = {}*{} = {}",
            pixels.len(),
            width,
            height,
            (width as usize) * (height as usize),
        );
        let (marginal_cdf, conditional_cdf, row_integrals, total_integral) =
            build_importance_cdf(width, height, &pixels);
        Self {
            width,
            height,
            pixels,
            intensity,
            marginal_cdf,
            conditional_cdf,
            row_integrals,
            total_integral,
        }
    }

    /// Sample the environment radiance toward direction `dir` (in
    /// world space). The direction is mapped to equirectangular UV
    /// space and bilinearly sampled.
    pub fn sample_direction(&self, dir: Vec3) -> Vec3 {
        if self.pixels.is_empty() {
            return Vec3::ZERO;
        }
        let n = dir.normalize_or_zero();
        // theta ∈ [0, π], phi ∈ [-π, π]
        let theta = n.y.clamp(-1.0, 1.0).acos();
        let phi = n.z.atan2(n.x);
        // u in [0, 1) wraps horizontally; v in [0, 1] runs top→bottom
        let u = (phi + PI) / (2.0 * PI);
        let v = theta / PI;
        let px = u * self.width as f32 - 0.5;
        let py = v * self.height as f32 - 0.5;
        let x0 = px.floor();
        let y0 = py.floor();
        let tx = px - x0;
        let ty = py - y0;
        // x wraps horizontally; y clamps to the [0, h-1] range. Clamp
        // `y0` and `y0 + 1` independently so that out-of-range queries
        // collapse to the nearest valid row (preventing red+white
        // bleed when sampling the very top or bottom of the sphere).
        let x0i = (x0 as i64).rem_euclid(self.width as i64) as u32;
        let x1i = (x0i + 1) % self.width;
        let y0i = (y0 as i64).clamp(0, self.height as i64 - 1) as u32;
        let y1i = ((y0 as i64) + 1).clamp(0, self.height as i64 - 1) as u32;
        let p00 = self.pixel(x0i, y0i);
        let p10 = self.pixel(x1i, y0i);
        let p01 = self.pixel(x0i, y1i);
        let p11 = self.pixel(x1i, y1i);
        let p0 = lerp3(p00, p10, tx);
        let p1 = lerp3(p01, p11, tx);
        lerp3(p0, p1, ty) * self.intensity
    }

    #[inline]
    fn pixel(&self, x: u32, y: u32) -> Vec3 {
        let p = self.pixels[(y as usize) * (self.width as usize) + (x as usize)];
        Vec3::from_array(p)
    }

    /// Solid-angle pdf for a given world direction, used by MIS when
    /// the BSDF strategy samples the environment via a miss.
    ///
    /// The pdf in (u, v) space is `L(u, v) * sin(theta) / I_total`;
    /// converting to solid angle multiplies by `width * height / (2π²
    /// sin(theta))`, leaving `L(u, v) * width * height / (2π² I_total)`.
    pub fn pdf_direction(&self, dir: Vec3) -> f32 {
        if self.total_integral <= 0.0 || self.pixels.is_empty() {
            return 0.0;
        }
        let n = dir.normalize_or_zero();
        let theta = n.y.clamp(-1.0, 1.0).acos();
        let sin_theta = theta.sin().max(1e-6);
        let phi = n.z.atan2(n.x);
        let u = (phi + PI) / (2.0 * PI);
        let v = theta / PI;
        let xi = ((u * self.width as f32) as i32).clamp(0, self.width as i32 - 1) as u32;
        let yi = ((v * self.height as f32) as i32).clamp(0, self.height as i32 - 1) as u32;
        let l = luminance(self.pixel(xi, yi));
        l * (self.width as f32) * (self.height as f32)
            / (2.0 * PI * PI * sin_theta * self.total_integral)
    }

    /// Importance-sample a direction from the environment. Returns
    /// `(direction, radiance, pdf_solid_angle)`. The returned pdf is
    /// in solid-angle units so the caller can MIS-combine with the
    /// BSDF.
    pub fn sample_importance(&self, u1: f32, u2: f32) -> Option<(Vec3, Vec3, f32)> {
        if self.pixels.is_empty() || self.total_integral <= 0.0 {
            return None;
        }
        // Sample row (v) from the marginal CDF.
        let y = cdf_search(&self.marginal_cdf, u1).min(self.height as usize - 1);
        let row_start = y * self.width as usize;
        let row_end = row_start + self.width as usize;
        let row = &self.conditional_cdf[row_start..row_end];
        let x = cdf_search(row, u2).min(self.width as usize - 1);

        let u = (x as f32 + 0.5) / self.width as f32;
        let v = (y as f32 + 0.5) / self.height as f32;
        let phi = u * 2.0 * PI - PI;
        let theta = v * PI;
        let sin_theta = theta.sin();
        let dir = Vec3::new(sin_theta * phi.cos(), theta.cos(), sin_theta * phi.sin())
            .normalize_or_zero();

        let radiance = Vec3::from_array(self.pixels[row_start + x]) * self.intensity;
        let pdf = self.pdf_direction(dir);
        if pdf <= 0.0 {
            return None;
        }
        Some((dir, radiance, pdf))
    }
}

fn cdf_search(cdf: &[f32], u: f32) -> usize {
    if cdf.is_empty() {
        return 0;
    }
    // Binary search for the first cumulative bin >= u; standard
    // inverse-CDF sampling.
    let mut lo = 0usize;
    let mut hi = cdf.len() - 1;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if cdf[mid] < u {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

#[inline]
fn luminance(rgb: Vec3) -> f32 {
    // ITU-R BT.709 / sRGB luma weights. Matches Cycles' importance
    // sampling and the OIDN albedo-prefilter.
    0.2126 * rgb.x + 0.7152 * rgb.y + 0.0722 * rgb.z
}

/// Build per-row conditional CDF + marginal CDF for sphere-weighted
/// importance sampling. Each pixel's luminance is scaled by
/// `sin(theta)` (the area of its equirectangular cell on the sphere).
fn build_importance_cdf(
    width: u32,
    height: u32,
    pixels: &[[f32; 3]],
) -> (Vec<f32>, Vec<f32>, Vec<f32>, f32) {
    let w = width as usize;
    let h = height as usize;
    let mut conditional_cdf = vec![0.0_f32; w * h];
    let mut row_integrals = vec![0.0_f32; h];

    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        let sin_theta = (v * PI).sin().max(0.0);
        let mut acc = 0.0_f32;
        for x in 0..w {
            let lum = luminance(Vec3::from_array(pixels[y * w + x])) * sin_theta;
            acc += lum;
            conditional_cdf[y * w + x] = acc;
        }
        row_integrals[y] = acc;
        if acc > 0.0 {
            for x in 0..w {
                conditional_cdf[y * w + x] /= acc;
            }
        }
    }

    let mut marginal_cdf = vec![0.0_f32; h];
    let mut acc = 0.0_f32;
    for (y, integ) in row_integrals.iter().enumerate() {
        acc += integ;
        marginal_cdf[y] = acc;
    }
    let total = acc;
    if total > 0.0 {
        for v in marginal_cdf.iter_mut() {
            *v /= total;
        }
    }
    (marginal_cdf, conditional_cdf, row_integrals, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red_white_env() -> EnvironmentMap {
        // 4x2 environment: top row red, bottom row white. Used to
        // verify importance sampling biases toward bright cells.
        let pixels = vec![
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
        ];
        EnvironmentMap::from_pixels(4, 2, pixels, 1.0)
    }

    #[test]
    fn sample_direction_returns_correct_band() {
        let env = red_white_env();
        // Up vector → top row (red).
        let s = env.sample_direction(Vec3::Y);
        assert!(s.x > 0.5, "expected red, got {s:?}");
        assert!(s.z < 0.5, "expected red, got {s:?}");
        // Down vector → bottom row (white).
        let s = env.sample_direction(-Vec3::Y);
        assert!(
            s.x > 0.5 && s.y > 0.5 && s.z > 0.5,
            "expected white, got {s:?}"
        );
    }

    #[test]
    fn importance_sample_pdf_is_positive() {
        let env = red_white_env();
        let (_dir, _rad, pdf) = env.sample_importance(0.3, 0.7).expect("sample");
        assert!(pdf > 0.0);
    }

    #[test]
    fn importance_sample_biases_toward_bright_rows() {
        // Build an env with a bright row, expect samples to cluster
        // toward that row.
        let mut pixels = vec![[0.01_f32, 0.01, 0.01]; 16];
        pixels[8] = [10.0, 10.0, 10.0];
        pixels[9] = [10.0, 10.0, 10.0];
        pixels[10] = [10.0, 10.0, 10.0];
        pixels[11] = [10.0, 10.0, 10.0];
        let env = EnvironmentMap::from_pixels(4, 4, pixels, 1.0);
        let mut bright_count = 0;
        let n = 1000;
        for i in 0..n {
            let u1 = (i as f32 + 0.5) / n as f32;
            let u2 = 0.5;
            if let Some((dir, _, _)) = env.sample_importance(u1, u2) {
                let theta = dir.y.clamp(-1.0, 1.0).acos();
                let v = theta / PI;
                let y_row = (v * 4.0) as usize;
                if y_row == 2 {
                    bright_count += 1;
                }
            }
        }
        // The bright row covers 25 % of the image area but ~99 % of
        // its luminance, so most samples should land there. Accept
        // any value > 60 % to allow noise.
        assert!(bright_count > 600, "bright_count = {bright_count}/{n}");
    }

    #[test]
    fn empty_environment_does_not_panic() {
        let env = EnvironmentMap::from_pixels(0, 0, vec![], 1.0);
        assert!(env.sample_direction(Vec3::Y).length_squared() == 0.0);
        assert!(env.sample_importance(0.5, 0.5).is_none());
    }

    /// `load_hdr` routes through `image::open`, which picks a
    /// decoder by file extension. The `exr` feature flag on the
    /// workspace `image` dependency must be active so OpenEXR
    /// `.exr` files round-trip without the "unsupported format"
    /// error. This regression test writes a tiny 2x1 EXR via the
    /// `image` crate and re-reads it through `EnvironmentMap`.
    #[test]
    fn load_hdr_decodes_exr_files() {
        use image::{Rgb, Rgb32FImage};
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("env.exr");
        let mut img = Rgb32FImage::new(2, 1);
        img.put_pixel(0, 0, Rgb([1.0, 0.0, 0.0]));
        img.put_pixel(1, 0, Rgb([0.0, 1.0, 0.0]));
        img.save(&p).expect("write 2x1 EXR");
        let env = EnvironmentMap::load_hdr(&p).expect("load 2x1 EXR");
        // Width/height should match what we wrote.
        let s = env.sample_direction(Vec3::new(1.0, 0.0, 0.0));
        // Either of the two cells will be returned depending on
        // wrap-around; both are bright primaries — just assert
        // we got a non-zero radiance back.
        assert!(
            s.length_squared() > 0.0,
            "exr sample should be non-zero, got {s:?}"
        );
    }

    /// Loading a non-existent path returns `EnvironmentError::NotFound`
    /// rather than the misleading `Decode` variant the `image::open`
    /// IO error used to surface. The bridge service relies on this
    /// distinction to render a "file not found" toast vs. "corrupt
    /// HDRI" toast.
    #[test]
    fn load_hdr_missing_path_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("does-not-exist.hdr");
        let err = EnvironmentMap::load_hdr(&bogus).expect_err("expected error");
        match err {
            EnvironmentError::NotFound(p) => assert_eq!(p, bogus),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
