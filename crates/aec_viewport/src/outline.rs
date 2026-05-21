//! Selection outline pass using jump-flood algorithm (JFA).
//!
//! Approach:
//!
//! 1. The navigable pipeline rasterises a *selection mask* — a single
//!    R8 texture where pixels that belong to a selected instance are
//!    `1.0` and everything else is `0.0`.
//! 2. We seed a "seed buffer" where each foreground pixel records its
//!    own coordinate, and each background pixel records `(-1, -1)`.
//! 3. JFA iterates `log2(max_outline_px)` passes, each one with step
//!    `step = max_outline_px / 2^pass`. At every pass each pixel
//!    samples its 8 neighbours at that step distance, keeping the
//!    seed closest to itself.
//! 4. The final SDF is the distance from each pixel to the nearest
//!    foreground pixel. Outline thickness in the composite is just
//!    `sdf <= width_px`.
//!
//! This module ships the CPU reference implementation (used for tests
//! and the headless fallback) so a missing-GPU CI run still validates
//! the algorithm. The GPU implementation in
//! [`crate::viewport_pipeline`] runs the same logic as compute / blit
//! passes; both share the same shader-style step structure so cross-
//! validation is straightforward.

use serde::{Deserialize, Serialize};

/// One pixel in the seed buffer — `Some((sx, sy))` for foreground (with
/// the absolute coords of the nearest seed found so far); `None` for
/// pixels that haven't seen any seed yet.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OutlinePixel {
    pub seed: Option<(i32, i32)>,
}

/// Result of the JFA: for each pixel the squared distance to the
/// nearest seed (or `u32::MAX` if no seed within the search radius).
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineSdf {
    pub width: u32,
    pub height: u32,
    pub squared_distances: Vec<u32>,
}

impl OutlineSdf {
    pub fn pixel_count(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }
}

/// Visual style for the composite stage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutlineStyle {
    /// Outline thickness in pixels.
    pub thickness_px: u32,
    /// Outline colour, premultiplied alpha (`[r, g, b, a]`).
    pub color_rgba: [f32; 4],
    /// When `true`, the foreground (selected pixels themselves) is left
    /// untouched. When `false`, the outline draws over the selected
    /// pixels too (giving a thicker silhouette).
    pub preserve_interior: bool,
}

impl Default for OutlineStyle {
    fn default() -> Self {
        Self {
            thickness_px: 2,
            color_rgba: [1.0, 0.6, 0.1, 1.0],
            preserve_interior: true,
        }
    }
}

/// Run the jump-flood algorithm on a binary mask. Input `mask` is a
/// `width × height` array of booleans (row-major); the output SDF has
/// one entry per pixel containing the squared distance from that pixel
/// to the nearest `true` mask pixel.
///
/// `max_step` controls how many JFA passes we do — the algorithm
/// converges in `O(log2(max_step))` passes. Default callers pass
/// `max_step = max(width, height).next_power_of_two() / 2`.
pub fn jump_flood_sdf(mask: &[bool], width: u32, height: u32, max_step: u32) -> OutlineSdf {
    assert_eq!(
        mask.len(),
        (width as usize).saturating_mul(height as usize),
        "mask must be width*height long"
    );
    let count = mask.len();
    let mut buf = vec![OutlinePixel::default(); count];
    // Seed: every foreground pixel stores its own coords.
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let i = (y as usize) * (width as usize) + (x as usize);
            if mask[i] {
                buf[i].seed = Some((x, y));
            }
        }
    }
    // JFA passes — step halves each pass.
    let mut step = max_step.max(1);
    while step >= 1 {
        let snapshot = buf.clone();
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let mut best = snapshot[(y as usize) * (width as usize) + (x as usize)];
                let mut best_d = match best.seed {
                    Some((sx, sy)) => squared_dist(x, y, sx, sy),
                    None => u32::MAX,
                };
                for dy in [-1, 0, 1] {
                    for dx in [-1, 0, 1] {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x + dx * step as i32;
                        let ny = y + dy * step as i32;
                        if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                            continue;
                        }
                        let neighbour = snapshot[(ny as usize) * (width as usize) + (nx as usize)];
                        if let Some((sx, sy)) = neighbour.seed {
                            let d = squared_dist(x, y, sx, sy);
                            if d < best_d {
                                best_d = d;
                                best.seed = Some((sx, sy));
                            }
                        }
                    }
                }
                let i = (y as usize) * (width as usize) + (x as usize);
                buf[i] = best;
            }
        }
        if step == 1 {
            break;
        }
        step /= 2;
    }
    let mut squared = vec![u32::MAX; count];
    for (i, px) in buf.iter().enumerate() {
        if let Some((sx, sy)) = px.seed {
            let x = (i % width as usize) as i32;
            let y = (i / width as usize) as i32;
            squared[i] = squared_dist(x, y, sx, sy);
        }
    }
    OutlineSdf {
        width,
        height,
        squared_distances: squared,
    }
}

fn squared_dist(x: i32, y: i32, sx: i32, sy: i32) -> u32 {
    let dx = (x - sx) as i64;
    let dy = (y - sy) as i64;
    (dx * dx + dy * dy).clamp(0, u32::MAX as i64) as u32
}

/// Generate a binary outline mask from a JFA SDF: `true` for pixels
/// within `style.thickness_px` of the seed set (inclusive).
pub fn outline_thickness_mask(sdf: &OutlineSdf, style: &OutlineStyle) -> Vec<bool> {
    let t2 = style.thickness_px.saturating_mul(style.thickness_px);
    sdf.squared_distances
        .iter()
        .map(|&d| {
            if d == 0 {
                // Foreground pixel.
                !style.preserve_interior
            } else {
                d <= t2
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_force_sdf(mask: &[bool], width: u32, height: u32) -> Vec<u32> {
        let count = mask.len();
        let mut out = vec![u32::MAX; count];
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let mut best = u32::MAX;
                for sy in 0..height as i32 {
                    for sx in 0..width as i32 {
                        let i = (sy as usize) * (width as usize) + (sx as usize);
                        if mask[i] {
                            let d = squared_dist(x, y, sx, sy);
                            if d < best {
                                best = d;
                            }
                        }
                    }
                }
                out[(y as usize) * (width as usize) + (x as usize)] = best;
            }
        }
        out
    }

    #[test]
    fn single_seed_in_center_produces_correct_distances() {
        let w = 5;
        let h = 5;
        let mut mask = vec![false; (w * h) as usize];
        mask[12] = true; // (2, 2)
        let sdf = jump_flood_sdf(&mask, w, h, 4);
        // Corners are at distance sqrt(8) -> squared = 8.
        assert_eq!(sdf.squared_distances[0], 8);
        assert_eq!(sdf.squared_distances[(w * h) as usize - 1], 8);
        assert_eq!(sdf.squared_distances[12], 0);
    }

    #[test]
    fn jfa_matches_brute_force_on_random_mask() {
        let w = 16;
        let h = 16;
        // Deterministic pseudo-random mask.
        let mut mask = vec![false; (w * h) as usize];
        for (i, slot) in mask.iter_mut().enumerate() {
            *slot = (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60) & 1 == 0;
        }
        // Ensure at least one seed.
        mask[42] = true;
        let sdf = jump_flood_sdf(&mask, w, h, 8);
        let brute = brute_force_sdf(&mask, w, h);
        for (i, (&a, &b)) in sdf.squared_distances.iter().zip(brute.iter()).enumerate() {
            assert_eq!(a, b, "pixel {i}: JFA gave {a} but brute-force gave {b}");
        }
    }

    #[test]
    fn empty_mask_leaves_all_distances_unset() {
        let w = 8;
        let h = 8;
        let mask = vec![false; (w * h) as usize];
        let sdf = jump_flood_sdf(&mask, w, h, 4);
        assert!(sdf.squared_distances.iter().all(|&d| d == u32::MAX));
    }

    #[test]
    fn full_mask_yields_all_zero_distances() {
        let w = 8;
        let h = 8;
        let mask = vec![true; (w * h) as usize];
        let sdf = jump_flood_sdf(&mask, w, h, 4);
        assert!(sdf.squared_distances.iter().all(|&d| d == 0));
    }

    #[test]
    fn outline_thickness_picks_pixels_within_two_pixels_radius() {
        let w = 5;
        let h = 5;
        let mut mask = vec![false; (w * h) as usize];
        mask[12] = true; // (2, 2)
        let sdf = jump_flood_sdf(&mask, w, h, 4);
        let style = OutlineStyle {
            thickness_px: 1,
            ..Default::default()
        };
        let outline = outline_thickness_mask(&sdf, &style);
        // (2, 2) is preserved (foreground), (1,2)/(2,1)/(3,2)/(2,3) are outline.
        assert!(!outline[12]);
        assert!(outline[11]); // (1, 2)
        assert!(outline[7]); // (2, 1)
        assert!(outline[13]); // (3, 2)
        assert!(outline[17]); // (2, 3)
                              // Corners are farther than thickness=1 -> not in outline.
        assert!(!outline[0]);
        assert!(!outline[(w * h - 1) as usize]);
    }

    #[test]
    fn outline_style_can_overpaint_interior_when_requested() {
        let w = 3;
        let h = 3;
        let mut mask = vec![false; (w * h) as usize];
        mask[4] = true; // (1, 1)
        let sdf = jump_flood_sdf(&mask, w, h, 2);
        let style = OutlineStyle {
            thickness_px: 1,
            preserve_interior: false,
            ..Default::default()
        };
        let outline = outline_thickness_mask(&sdf, &style);
        assert!(
            outline[4],
            "interior pixel is part of outline when overpaint is on"
        );
    }

    #[test]
    fn outline_pixel_default_has_no_seed() {
        let p = OutlinePixel::default();
        assert!(p.seed.is_none());
    }
}
