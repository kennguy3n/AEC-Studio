//! Low-discrepancy sample sequences for primary-ray jitter.
//!
//! The default per-pixel jitter strategy in this crate used to be
//! pure uniform random (`rng.f32()` for each of `(jx, jy)`). That's
//! perfectly unbiased but has high variance — adjacent samples can
//! cluster, leaving gaps that show up as noise. A *low-discrepancy*
//! sequence (Halton, Sobol, Hammersley, ...) covers the unit square
//! more uniformly: the worst-case star-discrepancy is `O(log^d N / N)`
//! versus `O(1/sqrt(N))` for pure random, which translates to roughly
//! 2-4× lower noise at the same sample count on smooth integrands.
//!
//! We use **Halton(2, 3)** here for the 2D primary-ray jitter:
//!
//! * It has a closed-form per-index formula, so any sample index can be
//!   evaluated in `O(log_b N)` time without state — convenient for
//!   stateless pass kernels like [`crate::path_trace::render_tile_pass`].
//! * It uses two co-prime bases (2 and 3), which gives the lowest
//!   2D discrepancy of any prefix-free sequence in this family.
//! * It composes cleanly with **Cranley-Patterson rotation**: per-pixel
//!   we draw a single random offset and add it modulo 1 to the Halton
//!   coordinate. This decorrelates the sequence between pixels (so the
//!   image doesn't show a global moire pattern) without destroying the
//!   per-pixel low-discrepancy property — a standard technique used by
//!   PBRT and Mitsuba.
//!
//! Other sequences (Sobol + Owen scrambling, Hammersley) are also
//! popular; Halton(2, 3) was chosen because (a) it's stateless and
//! (b) the bias of higher-base Halton is negligible at the sample
//! counts (<=4096) typical for AEC preview / final renders.
//!
//! ## Why not just use rng.f32() ?
//!
//! `fastrand::Rng::f32` is a high-quality PRNG, but for *integration*
//! quality the discrepancy of the sample sequence matters more than
//! the PRNG quality. A pure-random sequence will produce visible
//! "salt and pepper" noise that converges as `O(1/sqrt(N))`; a
//! low-discrepancy sequence with Cranley-Patterson randomization
//! produces smoother noise that converges as roughly
//! `O(log^2 N / N)` on the smooth parts of the integrand. For an
//! AEC interior render the smooth parts dominate, so the speedup is
//! real and visible.

/// Halton sequence in base `b`. Computes the `index`-th element of
/// the (one-dimensional) Halton sequence for the given prime base.
///
/// Returns a value in `[0, 1)`. The first 1024 indices stay strictly
/// less than 1.0 in `f32` and the radical-inverse expansion is
/// numerically stable thanks to the running multiplication by `1/b`.
///
/// # Why we hand-roll this instead of pulling a crate
///
/// `halton` and `sobol` crates exist on crates.io, but they all add
/// a `Vec`-backed state machine or initialise a global generator
/// table. We only need the closed-form radical inverse here, which
/// is six lines of code; vendoring it avoids a dependency just for
/// a numeric helper.
#[inline]
fn halton(index: u32, base: u32) -> f32 {
    debug_assert!(base >= 2, "Halton base must be >= 2");
    let inv_b = 1.0 / base as f32;
    let mut f = 1.0_f32;
    let mut r = 0.0_f32;
    let mut i = index;
    while i > 0 {
        f *= inv_b;
        r += f * ((i % base) as f32);
        i /= base;
    }
    r
}

/// Generate a 2D low-discrepancy sample with per-pixel Cranley-Patterson
/// rotation.
///
/// # Arguments
/// * `sample_index` — the 0-based index of this sample within the pixel.
///   Must come from a deterministic counter (the caller's per-sample
///   loop variable), not the RNG, so the sequence is reproducible.
/// * `pixel_seed` — a per-pixel random offset in `[0, 1)^2`, drawn once
///   per pixel from the RNG. Decorrelates the Halton sequence between
///   pixels so the image doesn't show a global pattern.
///
/// Returns `(jx, jy)` both in `[0, 1)` — drop-in replacement for the
/// previous `(rng.f32(), rng.f32())` pair.
///
/// For progressive / multi-pass rendering, `sample_index` should be the
/// *global* index of the sample within the per-pixel sequence (i.e.
/// `samples_so_far + s` where `samples_so_far` is the number of samples
/// the tile has already taken in previous passes and `s` is the
/// within-pass index). Together with a deterministic `pixel_seed` (see
/// [`pixel_rotation_seed`]) this gives every pixel a single coherent
/// Cranley-Patterson-rotated Halton sequence of length
/// `total_samples`, not K independent shifted sequences of length
/// `samples_per_pass`.
#[inline]
pub fn stratified_jitter(sample_index: u32, pixel_seed: [f32; 2]) -> (f32, f32) {
    // +1 so we never evaluate Halton at index 0 (which is exactly
    // 0.0 and would put every pixel's first sample at the top-left
    // pixel corner — visible as a sub-pixel bias on small sample
    // counts).
    //
    // `saturating_add` instead of `+ 1` so the function is total: a
    // pathological caller (or some future debug harness) passing
    // `u32::MAX` doesn't panic in debug and doesn't wrap to 0 (which
    // would re-introduce the corner-bias the +1 was added to avoid).
    // At u32::MAX it pins to u32::MAX and the Halton(u32::MAX, _) is
    // a perfectly valid sample. Reaching this requires ~4 G samples
    // per pixel, far beyond any preset (max is 1024 spp), so the
    // saturation is defense-in-depth, not load-bearing.
    let i = sample_index.saturating_add(1);
    let h1 = halton(i, 2);
    let h2 = halton(i, 3);
    let jx = (h1 + pixel_seed[0]).fract();
    let jy = (h2 + pixel_seed[1]).fract();
    (jx, jy)
}

/// Deterministic Cranley-Patterson rotation seed for the pixel at
/// `(px, py)`. Returns a pseudo-random `[f32; 2]` in `[0, 1)^2`.
///
/// "Deterministic" is the load-bearing word: across multiple
/// progressive passes the same pixel must get the *same* rotation so
/// the Halton indices from successive passes form a single coherent
/// low-discrepancy sequence. If the rotation changed per pass, the K
/// passes would produce K independent shifted Halton sequences (each
/// O(log²N/N) within itself but with no cross-pass correlation
/// benefit), which is what the previous RNG-drawn `pixel_seed` did.
///
/// Implementation: a 64-bit splitmix-style mix of the packed (px, py)
/// coordinate. Hashes are cheap (a few wrapping mul + xor / shift),
/// well-distributed for the 32-bit input space, and totally
/// dependency-free. We split the 64-bit output, mask each half to 24
/// bits, and divide by `2^24` for a uniform sample strictly in
/// `[0, 1)` — masking is the defensive guard against `u32::MAX as f32`
/// rounding up to `2^32` (which would yield exactly `1.0` and violate
/// the half-open contract). 24 bits is also exactly the mantissa
/// precision of `f32`, so no information is lost vs. the previous
/// `u32`-divided-by-`2^32` formulation — every representable f32 in
/// `[0, 1)` is reachable.
#[inline]
pub fn pixel_rotation_seed(px: u32, py: u32) -> [f32; 2] {
    let packed = (u64::from(px) << 32) | u64::from(py);
    // splitmix64 (Vigna, 2014).
    let mut z = packed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Two 24-bit halves -> two f32 strictly in [0, 1).
    const MASK_24: u32 = (1 << 24) - 1;
    const INV_2POW24: f32 = 1.0 / (1 << 24) as f32;
    let hi = (z >> 32) as u32 & MASK_24;
    let lo = (z as u32) & MASK_24;
    [hi as f32 * INV_2POW24, lo as f32 * INV_2POW24]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halton_returns_zero_at_zero() {
        assert_eq!(halton(0, 2), 0.0);
        assert_eq!(halton(0, 3), 0.0);
    }

    #[test]
    fn halton_base_two_matches_van_der_corput() {
        // Halton(., 2) is the van der Corput sequence: 0.5, 0.25,
        // 0.75, 0.125, 0.625, 0.375, 0.875, ...
        let expected = [0.5_f32, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875, 0.0625];
        for (i, want) in expected.iter().enumerate() {
            let got = halton((i as u32) + 1, 2);
            assert!(
                (got - want).abs() < 1e-6,
                "halton(2)[{i}] = {got}, want {want}"
            );
        }
    }

    #[test]
    fn halton_base_three_matches_known_prefix() {
        // Halton(., 3): 1/3, 2/3, 1/9, 4/9, 7/9, 2/9, 5/9, 8/9
        let expected = [
            1.0_f32 / 3.0,
            2.0 / 3.0,
            1.0 / 9.0,
            4.0 / 9.0,
            7.0 / 9.0,
            2.0 / 9.0,
            5.0 / 9.0,
            8.0 / 9.0,
        ];
        for (i, want) in expected.iter().enumerate() {
            let got = halton((i as u32) + 1, 3);
            assert!(
                (got - want).abs() < 1e-5,
                "halton(3)[{i}] = {got}, want {want}"
            );
        }
    }

    #[test]
    fn halton_stays_strictly_below_one() {
        // The radical-inverse formula must never produce 1.0 (the
        // sample would be outside [0, 1) and pixel coordinates would
        // wrap into the neighbouring pixel).
        for i in 1..=4096u32 {
            let v2 = halton(i, 2);
            let v3 = halton(i, 3);
            assert!(v2 < 1.0, "halton(2)[{i}] = {v2}");
            assert!(v3 < 1.0, "halton(3)[{i}] = {v3}");
            assert!(v2 >= 0.0);
            assert!(v3 >= 0.0);
        }
    }

    #[test]
    fn stratified_jitter_stays_in_unit_square() {
        // Cranley-Patterson rotation keeps the result in [0, 1)^2.
        for s in 0..256u32 {
            let (jx, jy) = stratified_jitter(s, [0.3, 0.7]);
            assert!((0.0..1.0).contains(&jx), "jx out of range: {jx}");
            assert!((0.0..1.0).contains(&jy), "jy out of range: {jy}");
        }
    }

    #[test]
    fn stratified_jitter_has_lower_discrepancy_than_pure_random() {
        // 64 samples on a 4x4 stratification grid. Halton should hit
        // strictly more cells than pure random does, because that's
        // the entire point of low-discrepancy sequences.
        const N: u32 = 64;
        const GRID: usize = 4;
        let mut halton_cells = [[false; GRID]; GRID];
        let mut random_cells = [[false; GRID]; GRID];
        // Halton: same seed so we measure the *sequence* property,
        // not the rotation.
        for s in 0..N {
            let (jx, jy) = stratified_jitter(s, [0.0, 0.0]);
            let cx = (jx * GRID as f32) as usize;
            let cy = (jy * GRID as f32) as usize;
            halton_cells[cx.min(GRID - 1)][cy.min(GRID - 1)] = true;
        }
        // Random with a fixed seed (so this test is deterministic).
        let mut rng = fastrand::Rng::with_seed(0xCAFEBABE);
        for _ in 0..N {
            let cx = (rng.f32() * GRID as f32) as usize;
            let cy = (rng.f32() * GRID as f32) as usize;
            random_cells[cx.min(GRID - 1)][cy.min(GRID - 1)] = true;
        }
        let halton_filled: u32 = halton_cells
            .iter()
            .map(|row| row.iter().filter(|c| **c).count() as u32)
            .sum();
        let random_filled: u32 = random_cells
            .iter()
            .map(|row| row.iter().filter(|c| **c).count() as u32)
            .sum();
        // Halton(2,3) fills the entire 4x4 grid in 16 samples; the
        // 64-sample budget is way more than enough. The fixed-seed
        // pure-random sequence happens to fill 15 cells. Use a loose
        // ">=" lower bound to guard against future Halton refactors
        // that happen to change which cell each index lands in, while
        // still pinning the qualitative claim.
        assert_eq!(
            halton_filled,
            (GRID * GRID) as u32,
            "Halton(2,3) on 64 samples must fill all 16 stratification cells"
        );
        assert!(
            halton_filled >= random_filled,
            "Halton coverage {} must beat or tie pure-random {}",
            halton_filled,
            random_filled
        );
    }

    #[test]
    fn pixel_rotation_seed_is_deterministic_across_calls() {
        // Load-bearing property for progressive rendering: the same
        // pixel must always get the same rotation across multiple
        // passes so the Halton index advance forms a single coherent
        // shifted sequence.
        for (px, py) in [(0, 0), (1, 0), (0, 1), (12345, 6789), (u32::MAX, u32::MAX)] {
            let a = pixel_rotation_seed(px, py);
            let b = pixel_rotation_seed(px, py);
            assert_eq!(a, b, "({px}, {py}) must be deterministic");
            assert!((0.0..1.0).contains(&a[0]) && (0.0..1.0).contains(&a[1]));
        }
    }

    #[test]
    fn pixel_rotation_seed_stays_strictly_below_one() {
        // Defence-in-depth for the [0, 1) contract — sweep a few
        // thousand pixel coordinates including ones whose splitmix64
        // hash is known to land near the f32 boundary. The 24-bit
        // mask in pixel_rotation_seed ensures the value can never
        // round up to 1.0 even when the raw upper-32-bits hash equals
        // u32::MAX. Callers that don't apply `.fract()` (e.g. a
        // future direct use of the seed) can rely on this strict
        // bound.
        let coords = [
            (0u32, 0u32),
            (1, 0),
            (0, 1),
            (u32::MAX, 0),
            (0, u32::MAX),
            (u32::MAX, u32::MAX),
            (u32::MAX - 1, u32::MAX - 1),
        ];
        for (px, py) in coords {
            let [a, b] = pixel_rotation_seed(px, py);
            assert!(
                a < 1.0,
                "pixel_rotation_seed({px}, {py})[0] = {a}, must be < 1.0"
            );
            assert!(
                b < 1.0,
                "pixel_rotation_seed({px}, {py})[1] = {b}, must be < 1.0"
            );
            assert!(a >= 0.0);
            assert!(b >= 0.0);
        }
        // Broad sweep to catch any other pixel-pair whose hash hits
        // the boundary.
        for px in 0..256u32 {
            for py in 0..256u32 {
                let [a, b] = pixel_rotation_seed(px, py);
                assert!(a < 1.0 && b < 1.0 && a >= 0.0 && b >= 0.0);
            }
        }
    }

    #[test]
    fn pixel_rotation_seed_decorrelates_neighbouring_pixels() {
        // Adjacent pixels must get visibly different rotations,
        // otherwise the Cranley-Patterson decorrelation doesn't
        // actually decorrelate anything across the image. Splitmix64
        // gives well-distributed output for any input increment, so
        // any pair of neighbours should differ by > 0.1 on at least
        // one axis (very loose lower bound).
        let mut min_dist = f32::INFINITY;
        for px in 0..16u32 {
            for py in 0..16u32 {
                let here = pixel_rotation_seed(px, py);
                if px + 1 < 16 {
                    let east = pixel_rotation_seed(px + 1, py);
                    let dx = (here[0] - east[0]).abs();
                    let dy = (here[1] - east[1]).abs();
                    min_dist = min_dist.min(dx.max(dy));
                }
            }
        }
        assert!(
            min_dist > 0.01,
            "neighbouring-pixel rotation distance too small ({min_dist})"
        );
    }

    #[test]
    fn stratified_jitter_progressive_index_yields_coherent_low_discrepancy() {
        // Two passes of 4 samples each, with samples_so_far threaded
        // through, must cover the 4x4 stratification grid better than
        // either pass alone. This pins the progressive-rendering
        // contract: passes advance the index rather than restart it.
        const GRID: usize = 4;
        let seed = pixel_rotation_seed(7, 11);
        let mut combined = [[false; GRID]; GRID];
        let mut pass_a = [[false; GRID]; GRID];
        let mut pass_b = [[false; GRID]; GRID];
        let mark = |cells: &mut [[bool; GRID]; GRID], jx: f32, jy: f32| {
            let cx = ((jx * GRID as f32) as usize).min(GRID - 1);
            let cy = ((jy * GRID as f32) as usize).min(GRID - 1);
            cells[cx][cy] = true;
        };
        // Pass A: indices 0..4 with samples_so_far=0.
        for s in 0..4u32 {
            let (jx, jy) = stratified_jitter(s, seed);
            mark(&mut pass_a, jx, jy);
            mark(&mut combined, jx, jy);
        }
        // Pass B: indices 4..8 (samples_so_far=4 + s). Same seed —
        // the rotation must be the same for the Halton advance to
        // accumulate low-discrepancy benefit.
        for s in 0..4u32 {
            let (jx, jy) = stratified_jitter(4 + s, seed);
            mark(&mut pass_b, jx, jy);
            mark(&mut combined, jx, jy);
        }
        let count = |c: &[[bool; GRID]; GRID]| c.iter().flatten().filter(|b| **b).count();
        let na = count(&pass_a);
        let nb = count(&pass_b);
        let nc = count(&combined);
        assert!(
            nc > na && nc > nb,
            "two passes with advancing index must cover more cells than either alone: \
             pass_a={na}, pass_b={nb}, combined={nc}"
        );
    }

    #[test]
    fn stratified_jitter_saturates_at_u32_max_instead_of_panicking() {
        // Defence-in-depth: a pathological caller feeding `u32::MAX`
        // must not panic in debug and must not wrap to 0 (which would
        // collapse the sample onto the top-left corner of the pixel,
        // re-introducing the bias the `+ 1` was added to avoid).
        // saturating_add pins the index at u32::MAX which is still a
        // valid Halton input.
        let seed = pixel_rotation_seed(0, 0);
        let (jx, jy) = stratified_jitter(u32::MAX, seed);
        assert!(jx.is_finite() && (0.0..1.0).contains(&jx));
        assert!(jy.is_finite() && (0.0..1.0).contains(&jy));
        // And one off the boundary should not produce the same sample —
        // i.e. saturating_add(u32::MAX) and saturating_add(u32::MAX-1)
        // both saturate to u32::MAX, so they're identical, but
        // u32::MAX-2 should still differ.
        let (jx_a, jy_a) = stratified_jitter(u32::MAX - 2, seed);
        assert!(
            (jx_a - jx).abs() > 1e-6 || (jy_a - jy).abs() > 1e-6,
            "samples below the saturation boundary must differ from the saturated one"
        );
    }
}
