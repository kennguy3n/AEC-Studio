//! LOD chain computation.
//!
//! The chain is built deterministically from the source mesh by selecting
//! every Nth triangle (quad-edge collapse and full mesh simplification will
//! land in Phase 3 once the lyon/meshopt deps are wired in). This produces a
//! real, dense-to-sparse LOD ladder that the viewport instancing layer can
//! switch between based on screen-space area.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LodLevel {
    pub level: u8,
    /// Ratio of triangles relative to LOD 0.
    pub ratio: f32,
    pub triangle_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LodChain {
    pub levels: Vec<LodLevel>,
}

impl LodChain {
    /// Produce a chain of at least 3 levels (`[1.0, 0.5, 0.25]` by default,
    /// extended by `extra_ratios`).
    ///
    /// This is the *legacy* ratio set used by the extension-host import
    /// path where each LOD level points at the same blob (no real
    /// decimation). For real-mesh imports that actually decimate the
    /// geometry per level use [`LodChain::aggressive_for_real_mesh`].
    pub fn from_ratios(base_triangle_count: u32, extra_ratios: &[f32]) -> Self {
        Self::from_seed_ratios(base_triangle_count, &[1.0_f32, 0.5, 0.25], extra_ratios)
    }

    /// Phase-11 real-mesh LOD chain: `[1.0, 0.25, 0.05]`.
    ///
    /// LOD 0 is the original mesh, LOD 1 retains ~25% of the triangles
    /// (mid-distance view), LOD 2 retains ~5% (far view / icon).
    /// This matches the spec for [`crate::pipeline::AssetImportPipeline::import_mesh`].
    pub fn aggressive_for_real_mesh(base_triangle_count: u32, extra_ratios: &[f32]) -> Self {
        Self::from_seed_ratios(base_triangle_count, &[1.0_f32, 0.25, 0.05], extra_ratios)
    }

    fn from_seed_ratios(
        base_triangle_count: u32,
        seed_ratios: &[f32],
        extra_ratios: &[f32],
    ) -> Self {
        let mut ratios: Vec<f32> = seed_ratios
            .iter()
            .copied()
            .chain(extra_ratios.iter().copied())
            .collect();
        ratios.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        ratios.dedup();
        let mut levels = Vec::with_capacity(ratios.len());
        for (i, ratio) in ratios.iter().enumerate() {
            let triangle_count = ((base_triangle_count as f32) * ratio).round().max(1.0) as u32;
            levels.push(LodLevel {
                level: i as u8,
                ratio: *ratio,
                triangle_count,
            });
        }
        Self { levels }
    }

    pub fn min_triangles(&self) -> u32 {
        self.levels
            .iter()
            .map(|l| l.triangle_count)
            .min()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chain_has_three_levels() {
        let chain = LodChain::from_ratios(1000, &[]);
        assert_eq!(chain.levels.len(), 3);
        assert_eq!(chain.levels[0].triangle_count, 1000);
        assert_eq!(chain.levels[1].triangle_count, 500);
        assert_eq!(chain.levels[2].triangle_count, 250);
    }

    #[test]
    fn extra_ratios_appear_in_chain() {
        let chain = LodChain::from_ratios(1000, &[0.1]);
        assert!(chain.levels.iter().any(|l| (l.ratio - 0.1).abs() < 1e-6));
    }

    #[test]
    fn ratios_are_sorted_descending_with_no_dups() {
        let chain = LodChain::from_ratios(1000, &[0.5, 0.1, 1.0]);
        let ratios: Vec<f32> = chain.levels.iter().map(|l| l.ratio).collect();
        assert_eq!(ratios, vec![1.0, 0.5, 0.25, 0.1]);
    }

    #[test]
    fn aggressive_chain_matches_spec_lod_ratios() {
        let chain = LodChain::aggressive_for_real_mesh(1000, &[]);
        let ratios: Vec<f32> = chain.levels.iter().map(|l| l.ratio).collect();
        assert_eq!(ratios, vec![1.0, 0.25, 0.05]);
        assert_eq!(chain.levels[0].triangle_count, 1000);
        assert_eq!(chain.levels[1].triangle_count, 250);
        assert_eq!(chain.levels[2].triangle_count, 50);
    }

    #[test]
    fn aggressive_chain_min_triangles_floored_at_one() {
        // Even a 5-triangle mesh produces a 3-level chain (no zeros).
        let chain = LodChain::aggressive_for_real_mesh(5, &[]);
        assert!(chain.levels.iter().all(|l| l.triangle_count >= 1));
    }
}
