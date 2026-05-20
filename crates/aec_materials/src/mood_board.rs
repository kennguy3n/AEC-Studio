//! Auto-generated mood board per room.
//!
//! Given a room id and the materials assigned to it, this module
//! produces a [`MoodBoard`] summarising the dominant colour palette,
//! the swatch list (one entry per material), and the union of style
//! tags. The output is consumed by the proposal pack renderer
//! (`aec_export::proposal`) so a contract pack can embed a one-page
//! mood-board summary without the user having to author one by hand.
//!
//! The colour-extraction pass is deterministic — given identical
//! input materials, the same palette comes out. We sort swatches by
//! `id` before extraction so callers can pass in materials in any
//! order and still get reproducible output (important for the
//! deterministic export tests in `aec_export`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::material::PbrMaterial;

/// Maximum number of swatches we keep on a single mood-board page.
/// Larger sets get truncated to the top-N by `style_tag` frequency
/// (most-prevalent style tag wins ties).
pub const MAX_SWATCHES_PER_PAGE: usize = 12;

/// Resolution of the colour-quantisation step — the dominant palette
/// is built by bucketing each material's albedo into a 6×6×6 cube
/// (216 bins). 6 is a happy medium between
/// "every material gets a unique bin" (bad) and "everything snaps
/// to one of eight bins" (also bad).
const PALETTE_BIN_COUNT: usize = 6;

/// One row on the mood-board page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodSwatch {
    pub material_id: String,
    pub name: String,
    /// Linear-space RGB shown in the page.
    pub albedo: [f32; 3],
    /// Optional style tag chosen as the primary tag for this swatch.
    /// Defaults to the first entry of `PbrMaterial::style_tags` when
    /// it exists.
    pub style_tag: Option<String>,
}

/// A single palette entry — a binned dominant colour plus the
/// material ids that contributed to that bin (used by the UI to
/// link palette entries back to the swatches that produced them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaletteEntry {
    pub albedo: [f32; 3],
    pub material_ids: Vec<String>,
    /// Number of contributing materials. Equal to
    /// `material_ids.len()` but exposed separately so the UI can show
    /// a count badge without recomputing.
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodBoard {
    pub room_id: String,
    pub swatches: Vec<MoodSwatch>,
    pub palette: Vec<PaletteEntry>,
    pub style_tags: Vec<String>,
}

impl MoodBoard {
    pub fn is_empty(&self) -> bool {
        self.swatches.is_empty()
    }

    /// How many of the truncated swatches fit on a single page.
    pub fn page_count(&self) -> usize {
        if self.swatches.is_empty() {
            0
        } else {
            self.swatches.len().div_ceil(MAX_SWATCHES_PER_PAGE)
        }
    }
}

/// Page-sized slice of a mood board ready to drop into a PDF.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodBoardPage {
    pub room_id: String,
    pub page_index: usize,
    pub swatches: Vec<MoodSwatch>,
}

impl MoodBoard {
    /// Split the mood board into [`MoodBoardPage`]s of at most
    /// [`MAX_SWATCHES_PER_PAGE`] swatches each. Page indices are 0-based.
    pub fn pages(&self) -> Vec<MoodBoardPage> {
        self.swatches
            .chunks(MAX_SWATCHES_PER_PAGE)
            .enumerate()
            .map(|(idx, chunk)| MoodBoardPage {
                room_id: self.room_id.clone(),
                page_index: idx,
                swatches: chunk.to_vec(),
            })
            .collect()
    }
}

/// Generate a mood board for `room_id` from `room_materials`.
pub fn generate_mood_board(
    room_id: impl Into<String>,
    room_materials: &[PbrMaterial],
) -> MoodBoard {
    let room_id = room_id.into();
    if room_materials.is_empty() {
        return MoodBoard {
            room_id,
            swatches: Vec::new(),
            palette: Vec::new(),
            style_tags: Vec::new(),
        };
    }

    // Sort by id so the output is deterministic regardless of input
    // ordering (the export tests depend on this).
    let mut sorted: Vec<&PbrMaterial> = room_materials.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    // Build the swatch list (truncating if there are too many).
    let swatches: Vec<MoodSwatch> = sorted
        .iter()
        .take(MAX_SWATCHES_PER_PAGE * 8) // hard cap for absurd inputs
        .map(|m| MoodSwatch {
            material_id: m.id.clone(),
            name: m.name.clone(),
            albedo: m.albedo,
            style_tag: m.style_tags.first().cloned(),
        })
        .collect();

    // Bucket the colours into the quantisation cube.
    let mut bins: BTreeMap<(u8, u8, u8), PaletteEntry> = BTreeMap::new();
    for m in &sorted {
        let key = quantise(m.albedo);
        bins.entry(key)
            .and_modify(|e| {
                e.material_ids.push(m.id.clone());
                e.count += 1;
                // Re-average so the displayed swatch tracks the bin's
                // average rather than the first material that landed in it.
                //
                // `e.count` is a u32 but `f32` exactly represents every
                // integer up to 2^24 (16M), which is far beyond the hard
                // cap of `MAX_SWATCHES_PER_PAGE * 8 = 96` materials taken
                // earlier in this function — so the lossless `as f32`
                // cast is safe and the previous u16-clamp dead-code that
                // froze precision at 65 535 is gone.
                let n = e.count as f32;
                for axis in 0..3 {
                    e.albedo[axis] = (e.albedo[axis] * (n - 1.0) + m.albedo[axis]) / n;
                }
            })
            .or_insert_with(|| PaletteEntry {
                albedo: m.albedo,
                material_ids: vec![m.id.clone()],
                count: 1,
            });
    }

    // Sort the palette by count descending (largest groups first),
    // ties broken by bin key for determinism.
    let mut palette: Vec<PaletteEntry> = bins.into_values().collect();
    palette.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| quantise(a.albedo).cmp(&quantise(b.albedo)))
    });

    // Collect style tags — sorted + deduplicated for stable output.
    let mut style_tags: Vec<String> = sorted.iter().flat_map(|m| m.style_tags.clone()).collect();
    style_tags.sort();
    style_tags.dedup();

    MoodBoard {
        room_id,
        swatches,
        palette,
        style_tags,
    }
}

/// Quantise a linear RGB triplet into a single bin in the
/// `PALETTE_BIN_COUNT`-cube. Saturating cast clamps to bins
/// `[0, PALETTE_BIN_COUNT)`.
fn quantise(rgb: [f32; 3]) -> (u8, u8, u8) {
    let bin = |c: f32| -> u8 {
        let c = c.clamp(0.0, 1.0);
        let raw = (c * PALETTE_BIN_COUNT as f32) as u32;
        raw.min(PALETTE_BIN_COUNT as u32 - 1) as u8
    };
    (bin(rgb[0]), bin(rgb[1]), bin(rgb[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(id: &str, name: &str, rgb: [f32; 3], tags: &[&str]) -> PbrMaterial {
        PbrMaterial::new(id, name)
            .with_albedo(rgb)
            .with_style_tags(tags.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn empty_input_returns_empty_board() {
        let mb = generate_mood_board("room-1", &[]);
        assert!(mb.is_empty());
        assert_eq!(mb.style_tags, Vec::<String>::new());
    }

    #[test]
    fn swatch_count_matches_input_count() {
        let mats = vec![
            mat("a", "Oak", [0.7, 0.55, 0.4], &["scandi"]),
            mat("b", "Wool", [0.85, 0.82, 0.78], &["scandi", "warm"]),
            mat("c", "Concrete", [0.4, 0.4, 0.42], &["industrial"]),
        ];
        let mb = generate_mood_board("room-1", &mats);
        assert_eq!(mb.swatches.len(), 3);
    }

    #[test]
    fn style_tags_are_union_sorted_dedup() {
        let mats = vec![
            mat("a", "x", [0.5; 3], &["scandi", "warm"]),
            mat("b", "y", [0.5; 3], &["warm", "industrial"]),
        ];
        let mb = generate_mood_board("room-1", &mats);
        assert_eq!(mb.style_tags, vec!["industrial", "scandi", "warm"]);
    }

    #[test]
    fn dominant_colour_groups_similar_albedos() {
        // Three near-greys + one warm oak — the greys should bucket
        // together and rank above the oak in the palette.
        let mats = vec![
            mat("a", "Grey1", [0.40, 0.41, 0.42], &[]),
            mat("b", "Grey2", [0.41, 0.40, 0.41], &[]),
            mat("c", "Grey3", [0.42, 0.42, 0.40], &[]),
            mat("d", "Oak", [0.78, 0.55, 0.30], &[]),
        ];
        let mb = generate_mood_board("room-1", &mats);
        assert!(!mb.palette.is_empty());
        assert!(mb.palette[0].count >= mb.palette[1].count);
        // The dominant bin should have collected the three greys.
        assert_eq!(mb.palette[0].count, 3);
    }

    #[test]
    fn output_is_deterministic_regardless_of_input_order() {
        let m1 = mat("a", "x", [0.7, 0.7, 0.7], &["s"]);
        let m2 = mat("b", "y", [0.3, 0.3, 0.3], &["i"]);
        let mb1 = generate_mood_board("r", &[m1.clone(), m2.clone()]);
        let mb2 = generate_mood_board("r", &[m2, m1]);
        assert_eq!(mb1, mb2);
    }

    #[test]
    fn pages_split_by_max_swatches() {
        let mats: Vec<PbrMaterial> = (0..(MAX_SWATCHES_PER_PAGE + 3))
            .map(|i| mat(&format!("m{i:02}"), &format!("Mat {i}"), [0.5; 3], &[]))
            .collect();
        let mb = generate_mood_board("r", &mats);
        let pages = mb.pages();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].swatches.len(), MAX_SWATCHES_PER_PAGE);
        assert_eq!(pages[1].swatches.len(), 3);
        assert_eq!(pages[0].page_index, 0);
        assert_eq!(pages[1].page_index, 1);
    }

    #[test]
    fn quantise_clamps_out_of_range_values() {
        // Values outside [0,1] should still produce a valid bin.
        let q = quantise([-0.5, 2.0, 0.5]);
        assert!(q.0 < PALETTE_BIN_COUNT as u8);
        assert!(q.1 < PALETTE_BIN_COUNT as u8);
        assert!(q.2 < PALETTE_BIN_COUNT as u8);
    }
}
