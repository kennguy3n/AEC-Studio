//! Render presets. Mirror the table in `ARCHITECTURE.md`:
//! Quick=32, Standard=128, High=256, Studio=1024.

use std::collections::BTreeMap;

use aec_governor::HardwareTier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderQuality {
    Eevee,
    Quick,
    Standard,
    High,
    Studio,
    Walkthrough,
    Panorama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPresetConfig {
    pub quality: RenderQuality,
    /// Sample count for Cycles; for EEVEE this is the temporal sample count.
    pub samples: u32,
    pub denoise: bool,
    pub tile_size_px: u32,
    pub resolution_x: u32,
    pub resolution_y: u32,
    pub use_motion_blur: bool,
    pub use_volumetric_atmosphere: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPreset {
    pub id: String,
    pub display_name: String,
    pub config: RenderPresetConfig,
}

impl RenderPreset {
    pub fn eevee_preview() -> Self {
        Self {
            id: "eevee_preview".into(),
            display_name: "EEVEE preview".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Eevee,
                samples: 64,
                denoise: false,
                tile_size_px: 256,
                resolution_x: 1280,
                resolution_y: 720,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn quick() -> Self {
        Self {
            id: "quick".into(),
            display_name: "Quick".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Quick,
                samples: 32,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1280,
                resolution_y: 720,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn standard() -> Self {
        Self {
            id: "standard".into(),
            display_name: "Standard".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Standard,
                samples: 128,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: false,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn high() -> Self {
        Self {
            id: "high".into(),
            display_name: "High".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::High,
                samples: 256,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: false,
                use_volumetric_atmosphere: true,
            },
        }
    }

    pub fn studio() -> Self {
        Self {
            id: "studio".into(),
            display_name: "Studio".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Studio,
                samples: 1024,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 3840,
                resolution_y: 2160,
                use_motion_blur: true,
                use_volumetric_atmosphere: true,
            },
        }
    }

    pub fn walkthrough() -> Self {
        Self {
            id: "walkthrough".into(),
            display_name: "Walkthrough".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Walkthrough,
                samples: 96,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 1920,
                resolution_y: 1080,
                use_motion_blur: true,
                use_volumetric_atmosphere: false,
            },
        }
    }

    pub fn panorama() -> Self {
        Self {
            id: "panorama".into(),
            display_name: "Panorama".into(),
            config: RenderPresetConfig {
                quality: RenderQuality::Panorama,
                samples: 512,
                denoise: true,
                tile_size_px: 256,
                resolution_x: 4096,
                resolution_y: 2048,
                use_motion_blur: false,
                use_volumetric_atmosphere: true,
            },
        }
    }

    /// All bundled presets.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self::eevee_preview(),
            Self::quick(),
            Self::standard(),
            Self::high(),
            Self::studio(),
            Self::walkthrough(),
            Self::panorama(),
        ]
    }

    /// Bundled preset for a given quality enum.
    pub fn from_quality(q: RenderQuality) -> Self {
        match q {
            RenderQuality::Eevee => Self::eevee_preview(),
            RenderQuality::Quick => Self::quick(),
            RenderQuality::Standard => Self::standard(),
            RenderQuality::High => Self::high(),
            RenderQuality::Studio => Self::studio(),
            RenderQuality::Walkthrough => Self::walkthrough(),
            RenderQuality::Panorama => Self::panorama(),
        }
    }
}

/// Pick the recommended Cycles preset for the given hardware tier per
/// ARCHITECTURE.md §10.2. Walkthrough/Panorama presets are
/// off-mainline (animation/360) so they're never the default
/// recommendation — they're selected explicitly by the user.
pub fn recommend_preset(tier: HardwareTier) -> RenderQuality {
    match tier {
        HardwareTier::Low => RenderQuality::Quick,
        HardwareTier::Medium => RenderQuality::Standard,
        HardwareTier::High => RenderQuality::High,
        HardwareTier::Pro => RenderQuality::Studio,
    }
}

/// Map a legacy `cycles_<quality>` preset id to its current short form.
///
/// Pre-2026-05 project packages serialised preset ids with the
/// `cycles_` prefix (e.g. `cycles_standard`). The new canonical
/// form drops the prefix to align with the TypeScript `RenderPresetKey`
/// union and the Blender worker names. Anywhere we look up a preset id
/// — in [`RenderPresetStore::get`] and inside the custom Deserialize
/// hook on [`RenderPresetStore::selected`] — we route through this
/// function so on-disk project files written by older builds keep
/// resolving without a phantom "Standard fallback" diff appearing in
/// the render history.
pub fn migrate_legacy_preset_id(id: &str) -> &str {
    match id {
        "cycles_quick" => "quick",
        "cycles_standard" => "standard",
        "cycles_high" => "high",
        "cycles_studio" => "studio",
        "cycles_walkthrough" => "walkthrough",
        "cycles_panorama" => "panorama",
        "cycles_eevee_preview" => "eevee_preview",
        other => other,
    }
}

/// Serde adapter that rewrites a legacy `cycles_*` preset id to the
/// canonical short form when deserialising. Lets old project packages
/// roundtrip cleanly through the new code without surprising the user
/// with a silent "Standard fallback" on load.
fn deserialize_migrated_preset_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(migrate_legacy_preset_id(&raw).to_string())
}

/// Persistent registry of render presets. Holds the bundled defaults plus
/// any user-defined custom presets. The currently selected preset is
/// stable across save/load cycles via the [`Self::selected`] field.
///
/// Serializes as a plain JSON object so the project package can store
/// it alongside the other domain stores (cameras, layers, schedules).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPresetStore {
    /// Custom (user-authored) presets keyed by id. The bundled defaults
    /// are kept in code and merged into [`Self::all`] on demand.
    pub custom: BTreeMap<String, RenderPreset>,
    /// The currently selected preset id. Always one of the bundled
    /// preset ids or a key from [`Self::custom`]. Old project files
    /// using the `cycles_*` prefix are migrated to the short form at
    /// deserialize time via [`migrate_legacy_preset_id`].
    #[serde(deserialize_with = "deserialize_migrated_preset_id")]
    pub selected: String,
    /// User override that pins the recommendation to a specific
    /// quality instead of using the hardware-tier derived default.
    /// When `None` the governor-recommended preset wins.
    #[serde(default)]
    pub recommendation_override: Option<RenderQuality>,
}

impl Default for RenderPresetStore {
    fn default() -> Self {
        Self {
            custom: BTreeMap::new(),
            selected: RenderPreset::standard().id,
            recommendation_override: None,
        }
    }
}

impl RenderPresetStore {
    /// Construct a store seeded with the bundled defaults and the
    /// recommendation for the given hardware tier preselected.
    pub fn for_tier(tier: HardwareTier) -> Self {
        let q = recommend_preset(tier);
        Self {
            custom: BTreeMap::new(),
            selected: RenderPreset::from_quality(q).id,
            recommendation_override: None,
        }
    }

    /// Bundled defaults + custom presets in insertion order.
    pub fn all(&self) -> Vec<RenderPreset> {
        let mut out = RenderPreset::defaults();
        out.extend(self.custom.values().cloned());
        out
    }

    /// Look up a preset by id. Bundled defaults take precedence over
    /// custom presets with the same id, so users cannot accidentally
    /// shadow a built-in preset. Legacy `cycles_*` ids are migrated to
    /// the canonical short form before lookup so old render-history
    /// entries (which embed the id) keep resolving after the rename.
    pub fn get(&self, id: &str) -> Option<RenderPreset> {
        let migrated = migrate_legacy_preset_id(id);
        RenderPreset::defaults()
            .into_iter()
            .find(|p| p.id == migrated)
            .or_else(|| self.custom.get(migrated).cloned())
    }

    /// Resolve the currently selected preset, falling back to
    /// `Standard` if the stored id no longer exists (e.g. after a
    /// removed custom preset).
    pub fn current(&self) -> RenderPreset {
        self.get(&self.selected)
            .unwrap_or_else(RenderPreset::standard)
    }

    /// Set the currently selected preset by id. Returns `false` if the
    /// id is unknown (in which case the selection is left untouched).
    pub fn select(&mut self, id: &str) -> bool {
        if self.get(id).is_some() {
            self.selected = id.to_string();
            true
        } else {
            false
        }
    }

    /// Insert or update a custom preset. The bundled default presets
    /// cannot be overridden — attempts to register a preset whose id
    /// collides with a built-in are rejected.
    pub fn insert_custom(&mut self, preset: RenderPreset) -> Result<(), PresetError> {
        if RenderPreset::defaults().iter().any(|p| p.id == preset.id) {
            return Err(PresetError::ReservedId(preset.id.clone()));
        }
        self.custom.insert(preset.id.clone(), preset);
        Ok(())
    }

    /// Remove a custom preset. Returns `true` if the preset existed and
    /// was removed. Cannot remove bundled defaults.
    pub fn remove_custom(&mut self, id: &str) -> bool {
        if self.custom.remove(id).is_some() {
            // If we just removed the active preset, fall back to the
            // bundled standard one so `current()` always resolves.
            if self.selected == id {
                self.selected = RenderPreset::standard().id;
            }
            true
        } else {
            false
        }
    }

    /// Return the preset recommended for the supplied hardware tier,
    /// taking [`Self::recommendation_override`] into account. The
    /// governor-recommended preset is the default; an override lets a
    /// user pin (for example) `Quick` on a Pro machine when iterating.
    pub fn recommended(&self, tier: HardwareTier) -> RenderPreset {
        let quality = self
            .recommendation_override
            .unwrap_or_else(|| recommend_preset(tier));
        RenderPreset::from_quality(quality)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetError {
    #[error("preset id `{0}` is reserved by a bundled preset")]
    ReservedId(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_increasing_samples() {
        assert!(RenderPreset::quick().config.samples < RenderPreset::standard().config.samples);
        assert!(RenderPreset::standard().config.samples < RenderPreset::high().config.samples);
        assert!(RenderPreset::high().config.samples < RenderPreset::studio().config.samples);
    }

    #[test]
    fn defaults_are_unique_ids() {
        let presets = RenderPreset::defaults();
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
    }

    #[test]
    fn recommend_preset_maps_each_tier_per_architecture_md() {
        // ARCHITECTURE.md §10.2 pins these mappings. The test ensures
        // tier-derived defaults never silently drift.
        assert_eq!(recommend_preset(HardwareTier::Low), RenderQuality::Quick);
        assert_eq!(
            recommend_preset(HardwareTier::Medium),
            RenderQuality::Standard
        );
        assert_eq!(recommend_preset(HardwareTier::High), RenderQuality::High);
        assert_eq!(recommend_preset(HardwareTier::Pro), RenderQuality::Studio);
    }

    #[test]
    fn store_for_tier_preselects_the_recommendation() {
        let store = RenderPresetStore::for_tier(HardwareTier::High);
        assert_eq!(store.current().config.quality, RenderQuality::High);
        let store_low = RenderPresetStore::for_tier(HardwareTier::Low);
        assert_eq!(store_low.current().config.quality, RenderQuality::Quick);
    }

    #[test]
    fn store_roundtrips_via_serde() {
        let mut store = RenderPresetStore::for_tier(HardwareTier::Pro);
        let mut custom = RenderPreset::standard();
        custom.id = "studio_high_iso".into();
        custom.display_name = "Studio (high ISO)".into();
        custom.config.samples = 768;
        store.insert_custom(custom.clone()).unwrap();
        assert!(store.select("studio_high_iso"));

        let bytes = serde_json::to_vec(&store).unwrap();
        let loaded: RenderPresetStore = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(loaded, store);
        assert_eq!(loaded.current().id, "studio_high_iso");
        assert_eq!(loaded.current().config.samples, 768);
    }

    #[test]
    fn store_rejects_custom_id_collisions_with_bundled() {
        let mut store = RenderPresetStore::default();
        let mut clash = RenderPreset::quick();
        clash.display_name = "User Quick".into();
        let err = store.insert_custom(clash).unwrap_err();
        assert!(matches!(err, PresetError::ReservedId(id) if id == "quick"));
    }

    #[test]
    fn remove_custom_falls_back_to_standard_for_active_preset() {
        let mut store = RenderPresetStore::default();
        let mut custom = RenderPreset::standard();
        custom.id = "draft_preset".into();
        store.insert_custom(custom).unwrap();
        store.select("draft_preset");
        assert_eq!(store.current().id, "draft_preset");
        assert!(store.remove_custom("draft_preset"));
        assert_eq!(store.current().id, RenderPreset::standard().id);
    }

    #[test]
    fn recommendation_override_pins_quality() {
        let mut store = RenderPresetStore::for_tier(HardwareTier::Pro);
        store.recommendation_override = Some(RenderQuality::Quick);
        assert_eq!(
            store.recommended(HardwareTier::Pro).config.quality,
            RenderQuality::Quick
        );
        // Without an override, the tier-based default is restored.
        store.recommendation_override = None;
        assert_eq!(
            store.recommended(HardwareTier::Pro).config.quality,
            RenderQuality::Studio
        );
    }

    #[test]
    fn store_select_rejects_unknown_ids() {
        let mut store = RenderPresetStore::default();
        let original = store.selected.clone();
        assert!(!store.select("nope"));
        assert_eq!(store.selected, original);
    }

    #[test]
    fn migrate_legacy_preset_id_maps_all_known_legacy_ids() {
        // Every preset that ever shipped with a `cycles_` prefix must
        // map to its current canonical short id; unknown ids pass
        // through unchanged.
        assert_eq!(migrate_legacy_preset_id("cycles_quick"), "quick");
        assert_eq!(migrate_legacy_preset_id("cycles_standard"), "standard");
        assert_eq!(migrate_legacy_preset_id("cycles_high"), "high");
        assert_eq!(migrate_legacy_preset_id("cycles_studio"), "studio");
        assert_eq!(migrate_legacy_preset_id("cycles_walkthrough"), "walkthrough");
        assert_eq!(migrate_legacy_preset_id("cycles_panorama"), "panorama");
        assert_eq!(
            migrate_legacy_preset_id("cycles_eevee_preview"),
            "eevee_preview"
        );
        // Already-canonical ids are untouched.
        assert_eq!(migrate_legacy_preset_id("standard"), "standard");
        // Unknown ids pass through (custom user presets, etc.).
        assert_eq!(migrate_legacy_preset_id("custom_evening"), "custom_evening");
    }

    #[test]
    fn store_deserializes_legacy_cycles_ids() {
        // Mimics a project package written before the 2026-05 rename:
        // the `selected` field uses the `cycles_` prefix. After load
        // it must resolve to a real preset, not silently fall back to
        // Standard, so the render history doesn't show a phantom diff.
        let legacy_json = r#"{
            "custom": {},
            "selected": "cycles_high",
            "recommendation_override": null
        }"#;
        let store: RenderPresetStore = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(store.selected, "high");
        assert_eq!(store.current().id, "high");
    }

    #[test]
    fn store_get_accepts_legacy_id_at_runtime() {
        // Defensive: even if a legacy id reaches `get()` at runtime
        // (e.g. from a render-history entry that bypassed deserialize
        // migration), it must resolve to the canonical preset rather
        // than returning None.
        let store = RenderPresetStore::default();
        let preset = store.get("cycles_studio").expect("legacy id resolves");
        assert_eq!(preset.id, "studio");
    }
}
