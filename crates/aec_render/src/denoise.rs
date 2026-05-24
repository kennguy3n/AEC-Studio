// Single-letter math identifiers (n, p, w, x, y, c) are used throughout
// the image-processing literature; expanding them obscures the algebra.
#![allow(clippy::many_single_char_names)]

//! Image-space denoiser for the path tracer.
//!
//! Two production-quality kernels live here:
//!
//! 1. [`bilateral_denoise`] — joint bilateral filter that consults the
//!    co-rendered normal and albedo aux buffers so it does **not** smear
//!    across geometric edges or albedo discontinuities. Cheap (5×5
//!    window) and runs over the whole image in parallel.
//! 2. [`nlm_denoise`] — non-local means filter using a small search
//!    window and patch-distance similarity. Higher quality at the cost
//!    of more work per pixel.
//!
//! The path tracer produces three buffers per pixel: the noisy color
//! estimate, the world-space normal at the first hit, and the surface
//! albedo. The aux buffers are *much* less noisy than the color buffer
//! and let us preserve edges robustly.
//!
//! Intel Open Image Denoise (OIDN) is a higher-quality option, but it
//! requires an FFI wrapper (unsafe) plus the OpenImageDenoise SDK
//! installed at link time. The AEC Studio workspace forbids
//! `unsafe_code` to keep the core crates auditable, and the SDK is not
//! packaged in standard Linux distro repositories. So OIDN is
//! intentionally NOT bundled with `aec_render` — third parties who
//! want OIDN can implement it in a separate crate that depends on
//! `aec_render::denoise` for the public types ([`ImageRgb`] etc.) and
//! plugs in via [`Denoiser`] (extensible via the public API).

use rayon::prelude::*;

/// Single-precision RGB image. Stored as `[r, g, b]` per pixel in
/// row-major order.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 3]>,
}

impl ImageRgb {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![[0.0; 3]; (width as usize) * (height as usize)],
        }
    }

    /// Construct from a flat slice of `[r, g, b]` triplets.
    pub fn from_pixels(width: u32, height: u32, pixels: Vec<[f32; 3]>) -> Self {
        assert_eq!(
            pixels.len(),
            (width as usize) * (height as usize),
            "ImageRgb::from_pixels length mismatch"
        );
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Sample variance of luminance across the entire image. Used by
    /// the unit tests to assert denoised output has lower noise.
    pub fn luma_variance(&self) -> f32 {
        let n = self.pixels.len() as f32;
        if n < 2.0 {
            return 0.0;
        }
        let mean: f32 = self
            .pixels
            .iter()
            .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
            .sum::<f32>()
            / n;
        let sumsq: f32 = self
            .pixels
            .iter()
            .map(|p| {
                let lum = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                (lum - mean) * (lum - mean)
            })
            .sum();
        sumsq / (n - 1.0)
    }

    fn idx(&self, x: i32, y: i32) -> usize {
        let cx = x.clamp(0, self.width as i32 - 1) as usize;
        let cy = y.clamp(0, self.height as i32 - 1) as usize;
        cy * (self.width as usize) + cx
    }
}

/// Parameters for the joint bilateral kernel. Sensible defaults are
/// picked for path-traced output at 1080p; tune `radius` / `sigma_*`
/// for faster / softer results.
#[derive(Debug, Clone, Copy)]
pub struct BilateralParams {
    /// Half-window radius in pixels. 2 → 5×5 window, 3 → 7×7 window.
    pub radius: i32,
    /// Spatial Gaussian sigma (pixels).
    pub sigma_space: f32,
    /// Range Gaussian sigma on luminance.
    pub sigma_color: f32,
    /// Range Gaussian sigma on the normal (dot-product space).
    pub sigma_normal: f32,
    /// Range Gaussian sigma on albedo.
    pub sigma_albedo: f32,
}

impl Default for BilateralParams {
    fn default() -> Self {
        Self {
            radius: 2,
            sigma_space: 2.0,
            sigma_color: 0.15,
            sigma_normal: 0.1,
            sigma_albedo: 0.1,
        }
    }
}

/// Joint bilateral filter. Edge-aware via normal + albedo guidance.
///
/// `normal` and `albedo` may be `None` — in that case the filter
/// degenerates to a luminance-only bilateral (still produces a useful
/// result, just less edge-preserving).
pub fn bilateral_denoise(
    color: &ImageRgb,
    normal: Option<&ImageRgb>,
    albedo: Option<&ImageRgb>,
    params: BilateralParams,
) -> ImageRgb {
    let w = color.width as i32;
    let h = color.height as i32;
    let r = params.radius.max(0);
    let inv_2sigma_space2 = 1.0 / (2.0 * params.sigma_space * params.sigma_space + 1.0e-6);
    let inv_2sigma_color2 = 1.0 / (2.0 * params.sigma_color * params.sigma_color + 1.0e-6);
    let inv_2sigma_normal2 = 1.0 / (2.0 * params.sigma_normal * params.sigma_normal + 1.0e-6);
    let inv_2sigma_albedo2 = 1.0 / (2.0 * params.sigma_albedo * params.sigma_albedo + 1.0e-6);

    let output: Vec<[f32; 3]> = (0..(w * h))
        .into_par_iter()
        .map(|i| {
            let cx = i % w;
            let cy = i / w;
            let center = color.pixels[color.idx(cx, cy)];
            let center_lum = 0.2126 * center[0] + 0.7152 * center[1] + 0.0722 * center[2];
            let center_n = normal.map(|n| n.pixels[n.idx(cx, cy)]);
            let center_a = albedo.map(|a| a.pixels[a.idx(cx, cy)]);
            let mut sum = [0.0_f32; 3];
            let mut total_weight = 0.0_f32;
            for dy in -r..=r {
                for dx in -r..=r {
                    let nx = cx + dx;
                    let ny = cy + dy;
                    let sample = color.pixels[color.idx(nx, ny)];
                    let sample_lum = 0.2126 * sample[0] + 0.7152 * sample[1] + 0.0722 * sample[2];
                    let space_term = ((dx * dx + dy * dy) as f32) * inv_2sigma_space2;
                    let color_term = (sample_lum - center_lum).powi(2) * inv_2sigma_color2;
                    let normal_term = match (center_n, normal) {
                        (Some(nc), Some(n)) => {
                            let ns = n.pixels[n.idx(nx, ny)];
                            // Dot product compared via `1 - dot` so a
                            // perfectly-aligned normal contributes 0
                            // distance.
                            let dot =
                                (nc[0] * ns[0] + nc[1] * ns[1] + nc[2] * ns[2]).clamp(-1.0, 1.0);
                            (1.0 - dot).powi(2) * inv_2sigma_normal2
                        }
                        _ => 0.0,
                    };
                    let albedo_term = match (center_a, albedo) {
                        (Some(ac), Some(a)) => {
                            let asamp = a.pixels[a.idx(nx, ny)];
                            let d = [ac[0] - asamp[0], ac[1] - asamp[1], ac[2] - asamp[2]];
                            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) * inv_2sigma_albedo2
                        }
                        _ => 0.0,
                    };
                    let weight = (-(space_term + color_term + normal_term + albedo_term)).exp();
                    sum[0] += weight * sample[0];
                    sum[1] += weight * sample[1];
                    sum[2] += weight * sample[2];
                    total_weight += weight;
                }
            }
            if total_weight <= 1.0e-10 {
                center
            } else {
                [
                    sum[0] / total_weight,
                    sum[1] / total_weight,
                    sum[2] / total_weight,
                ]
            }
        })
        .collect();

    ImageRgb {
        width: color.width,
        height: color.height,
        pixels: output,
    }
}

/// Non-local means parameters.
#[derive(Debug, Clone, Copy)]
pub struct NlmParams {
    /// Half-size of the search window in pixels. Total window =
    /// `(2*search+1)²`.
    pub search_radius: i32,
    /// Half-size of the patch used for similarity. Total patch =
    /// `(2*patch+1)²`.
    pub patch_radius: i32,
    /// Range parameter — smaller = preserve more detail, larger =
    /// smoother. Roughly the expected noise sigma.
    pub h: f32,
}

impl Default for NlmParams {
    fn default() -> Self {
        Self {
            search_radius: 3,
            patch_radius: 1,
            h: 0.15,
        }
    }
}

/// Non-local-means denoiser. Operates on luminance similarity between
/// patches; aux buffers are NOT used here (NLM is already robust on
/// natural images, and the bilateral variant covers the
/// edge-preservation use case).
pub fn nlm_denoise(color: &ImageRgb, params: NlmParams) -> ImageRgb {
    let w = color.width as i32;
    let h = color.height as i32;
    let sr = params.search_radius.max(0);
    let pr = params.patch_radius.max(0);
    let patch_size = ((2 * pr + 1) * (2 * pr + 1)).max(1) as f32;
    let inv_h2 = 1.0 / (params.h * params.h * patch_size + 1.0e-6);

    let output: Vec<[f32; 3]> = (0..(w * h))
        .into_par_iter()
        .map(|i| {
            let cx = i % w;
            let cy = i / w;
            let mut total_weight = 0.0_f32;
            let mut sum = [0.0_f32; 3];
            for dy in -sr..=sr {
                for dx in -sr..=sr {
                    let nx = cx + dx;
                    let ny = cy + dy;
                    let mut patch_dist = 0.0_f32;
                    for py in -pr..=pr {
                        for px in -pr..=pr {
                            let pc = color.pixels[color.idx(cx + px, cy + py)];
                            let pn = color.pixels[color.idx(nx + px, ny + py)];
                            let dr = pc[0] - pn[0];
                            let dg = pc[1] - pn[1];
                            let db = pc[2] - pn[2];
                            patch_dist += dr * dr + dg * dg + db * db;
                        }
                    }
                    let weight = (-patch_dist * inv_h2).exp();
                    let sample = color.pixels[color.idx(nx, ny)];
                    sum[0] += weight * sample[0];
                    sum[1] += weight * sample[1];
                    sum[2] += weight * sample[2];
                    total_weight += weight;
                }
            }
            if total_weight <= 1.0e-10 {
                color.pixels[color.idx(cx, cy)]
            } else {
                [
                    sum[0] / total_weight,
                    sum[1] / total_weight,
                    sum[2] / total_weight,
                ]
            }
        })
        .collect();

    ImageRgb {
        width: color.width,
        height: color.height,
        pixels: output,
    }
}

/// Available denoiser backends. All variants are pure-Rust — no
/// external runtime libraries required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denoiser {
    /// Edge-aware joint bilateral filter (default).
    Bilateral,
    /// Non-local means.
    Nlm,
}

impl Denoiser {
    /// Run the selected denoiser on the supplied buffers and return
    /// the filtered color image.
    pub fn apply(
        self,
        color: &ImageRgb,
        normal: Option<&ImageRgb>,
        albedo: Option<&ImageRgb>,
    ) -> ImageRgb {
        match self {
            Denoiser::Bilateral => {
                bilateral_denoise(color, normal, albedo, BilateralParams::default())
            }
            Denoiser::Nlm => nlm_denoise(color, NlmParams::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesise a noisy gradient image plus a clean normal buffer that
    /// signals a sharp edge at x=width/2. Used to validate that the
    /// bilateral filter preserves the edge.
    fn synthesise_noisy_edge(
        width: u32,
        height: u32,
        noise_amplitude: f32,
    ) -> (ImageRgb, ImageRgb) {
        let mut color = ImageRgb::new(width, height);
        let mut normal = ImageRgb::new(width, height);
        let mut rng = fastrand::Rng::with_seed(0xC0FFEE);
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) as usize;
                let half = width / 2;
                let base = if x < half { 0.2 } else { 0.8 };
                let n = (rng.f32() - 0.5) * 2.0 * noise_amplitude;
                color.pixels[i] = [
                    (base + n).clamp(0.0, 1.0),
                    (base + n).clamp(0.0, 1.0),
                    (base + n).clamp(0.0, 1.0),
                ];
                // Normal flips at the edge — left half points +X, right
                // half points -X.
                normal.pixels[i] = if x < half {
                    [1.0, 0.0, 0.0]
                } else {
                    [-1.0, 0.0, 0.0]
                };
            }
        }
        (color, normal)
    }

    #[test]
    fn bilateral_reduces_variance() {
        let (color, normal) = synthesise_noisy_edge(64, 64, 0.15);
        let noisy_var = color.luma_variance();
        let denoised = bilateral_denoise(&color, Some(&normal), None, BilateralParams::default());
        let clean_var = denoised.luma_variance();
        // The variance after filtering should still reflect the
        // bimodal distribution (0.2 vs 0.8), so it's NOT zero — but
        // it should be lower than the noisy input within each half.
        // Measure local variance in the left half alone:
        let half = 64u32 / 2;
        let left_noisy: Vec<[f32; 3]> = color
            .pixels
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if (i as u32 % 64) < half {
                    Some(*p)
                } else {
                    None
                }
            })
            .collect();
        let left_clean: Vec<[f32; 3]> = denoised
            .pixels
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if (i as u32 % 64) < half {
                    Some(*p)
                } else {
                    None
                }
            })
            .collect();
        let var_noisy_left = ImageRgb::from_pixels(half, 64, left_noisy).luma_variance();
        let var_clean_left = ImageRgb::from_pixels(half, 64, left_clean).luma_variance();
        assert!(
            var_clean_left < var_noisy_left * 0.5,
            "left-half variance should drop ≥2× (noisy={var_noisy_left}, clean={var_clean_left}, global noisy={noisy_var}, global clean={clean_var})"
        );
    }

    #[test]
    fn bilateral_preserves_edge_with_normal_guidance() {
        let (color, normal) = synthesise_noisy_edge(64, 64, 0.05);
        let denoised = bilateral_denoise(
            &color,
            Some(&normal),
            None,
            BilateralParams {
                radius: 3,
                sigma_space: 4.0,
                sigma_color: 1.0, // very lax color sigma
                sigma_normal: 0.05,
                sigma_albedo: 0.5,
            },
        );
        // Sample one pixel just inside the left half and one just
        // inside the right half. They should remain ~0.2 and ~0.8.
        let left = denoised.pixels[(32 * 64 + 30) as usize];
        let right = denoised.pixels[(32 * 64 + 33) as usize];
        assert!(left[0] < 0.45, "left edge should stay dark, got {left:?}");
        assert!(
            right[0] > 0.55,
            "right edge should stay bright, got {right:?}"
        );
    }

    #[test]
    fn nlm_reduces_variance_of_constant_field() {
        // Flat image plus white noise — NLM should average it back to
        // the underlying mean.
        let mut rng = fastrand::Rng::with_seed(42);
        let mut color = ImageRgb::new(32, 32);
        for p in color.pixels.iter_mut() {
            let n = (rng.f32() - 0.5) * 0.2;
            *p = [0.5 + n, 0.5 + n, 0.5 + n];
        }
        let noisy_var = color.luma_variance();
        let denoised = nlm_denoise(
            &color,
            NlmParams {
                search_radius: 4,
                patch_radius: 1,
                h: 0.1,
            },
        );
        let clean_var = denoised.luma_variance();
        assert!(
            clean_var < noisy_var * 0.5,
            "NLM should reduce variance ≥2× on a flat field (noisy={noisy_var}, clean={clean_var})"
        );
    }

    #[test]
    fn bilateral_without_aux_buffers_still_runs() {
        let mut rng = fastrand::Rng::with_seed(7);
        let mut color = ImageRgb::new(16, 16);
        for p in color.pixels.iter_mut() {
            let n = (rng.f32() - 0.5) * 0.5;
            *p = [(0.3 + n).max(0.0); 3];
        }
        let noisy_var = color.luma_variance();
        let out = bilateral_denoise(&color, None, None, BilateralParams::default());
        let var = out.luma_variance();
        assert!(
            var <= noisy_var,
            "even without aux buffers should not increase variance"
        );
    }

    #[test]
    fn denoiser_enum_dispatches_correctly() {
        let mut rng = fastrand::Rng::with_seed(3);
        let mut color = ImageRgb::new(16, 16);
        for p in color.pixels.iter_mut() {
            *p = [rng.f32(); 3];
        }
        let out_b = Denoiser::Bilateral.apply(&color, None, None);
        let out_n = Denoiser::Nlm.apply(&color, None, None);
        assert_eq!(out_b.width, 16);
        assert_eq!(out_n.height, 16);
        // Both must produce SOMETHING; specifically the variance
        // shouldn't blow up beyond the input.
        let in_var = color.luma_variance();
        assert!(out_b.luma_variance() <= in_var * 1.05);
        assert!(out_n.luma_variance() <= in_var * 1.05);
    }

    #[test]
    fn image_idx_clamps_out_of_bounds_samples() {
        let img = ImageRgb::new(4, 4);
        // Out-of-bounds samples must clamp into the image so the
        // bilateral/NLM kernels never read past the end of the buffer.
        let i = img.idx(-5, 100);
        assert!(i < img.pixels.len());
    }

    #[test]
    fn from_pixels_roundtrips() {
        let pix = vec![[1.0, 2.0, 3.0]; 9];
        let img = ImageRgb::from_pixels(3, 3, pix.clone());
        assert_eq!(img.pixels, pix);
    }

    /// Pins the contract that `bilateral_denoise`'s second argument is
    /// the *normal* image and the third is the *albedo* image. The two
    /// slots are not interchangeable because the kernel uses
    /// structurally different formulas:
    ///
    /// * normal-edge term — `(1 − dot(n_c, n_s))² · inv_2σ²_normal`,
    ///   tuned for unit-length vectors in `[-1, 1]`.
    /// * albedo-edge term — `‖a_c − a_s‖² · inv_2σ²_albedo`, tuned for
    ///   RGB triples in `[0, 1]`.
    ///
    /// If a future refactor accidentally swaps them at any call site
    /// (the failure mode flagged by Devin Review in PR-J round-2 on the
    /// `encode_srgb8` call), feeding the same data into the wrong slot
    /// produces a different distance metric — so two calls with the
    /// aux images in opposite slots must produce a measurably different
    /// output. This test makes that property explicit.
    #[test]
    fn bilateral_normal_and_albedo_slots_are_not_interchangeable() {
        // Color: 5×5 sinusoid in luminance so the kernel has something
        // to weight. Per-pixel value depends on (x + y) to break any
        // accidental symmetry between row-major and column-major.
        let w = 5_u32;
        let h = 5_u32;
        let pixels: Vec<[f32; 3]> = (0..(w * h))
            .map(|i| {
                let x = (i % w) as f32;
                let y = (i / w) as f32;
                let v = 0.5 + 0.3 * ((x * 0.7 + y * 1.3).sin());
                [v, v, v]
            })
            .collect();
        let color = ImageRgb::from_pixels(w, h, pixels);
        // `n_like` is shaped like a real normal buffer — components
        // in `[-1, 1]` with magnitudes near 1.
        let n_like = ImageRgb::from_pixels(
            w,
            h,
            (0..(w * h))
                .map(|i| {
                    let t = i as f32 * 0.31;
                    let nx = t.sin();
                    let ny = t.cos();
                    let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
                    [nx, ny, nz]
                })
                .collect(),
        );
        // `a_like` is shaped like an albedo buffer — RGB in `[0, 1]`
        // with deliberately different spatial variation than `n_like`.
        let a_like = ImageRgb::from_pixels(
            w,
            h,
            (0..(w * h))
                .map(|i| {
                    let t = i as f32 * 0.17;
                    [0.5 + 0.4 * t.sin(), 0.5 + 0.4 * t.cos(), 0.5]
                })
                .collect(),
        );

        // Use sigmas large enough that BOTH terms contribute
        // measurably to the weights (otherwise the exp() saturates and
        // the swap is invisible).
        let params = BilateralParams {
            radius: 2,
            sigma_space: 2.0,
            sigma_color: 0.5,
            sigma_normal: 0.6,
            sigma_albedo: 0.6,
        };
        let correct = bilateral_denoise(&color, Some(&n_like), Some(&a_like), params);
        let swapped = bilateral_denoise(&color, Some(&a_like), Some(&n_like), params);

        let mut max_abs_diff = 0.0_f32;
        for (c, s) in correct.pixels.iter().zip(swapped.pixels.iter()) {
            for ch in 0..3 {
                max_abs_diff = max_abs_diff.max((c[ch] - s[ch]).abs());
            }
        }
        // The two outputs MUST differ by more than floating-point
        // noise — otherwise the kernel would be symmetric in
        // `(normal, albedo)` and the call-site argument order would
        // be meaningless.
        assert!(
            max_abs_diff > 1.0e-3,
            "bilateral_denoise must distinguish (normal, albedo) from \
             (albedo, normal); got max_abs_diff = {max_abs_diff}"
        );
    }
}
