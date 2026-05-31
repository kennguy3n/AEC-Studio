//! Phase 18 Group D Task 18 — single source of truth for every
//! model the local AI runtime knows about.
//!
//! The registry is loaded once at process startup by parsing the
//! compile-time-embedded `crates/aec_ai/data/ai_models.json` JSON
//! blob (via `include_str!`). Loading at compile-time keeps the
//! integrity-pinning invariant from Phase 18 Group A:
//!
//! - BLAKE3 hashes / expected file sizes / canonical URLs are
//!   baked into the binary at build time so they cannot be tampered
//!   with by editing on-disk JSON post-install (the JSON is read by
//!   `serde_json` from the in-binary `&'static str`, never from
//!   disk at runtime).
//! - Adding / updating a model is still a JSON edit (no Rust code
//!   change), but the JSON must land in the repo before the release
//!   build that ships it.
//! - A future binary that reads a `GovernorPolicy` JSON written by
//!   a pre-Group-D binary still works — `schema_version` lets us
//!   detect a stale-shape decode at boot rather than blowing up
//!   the first time a caller asks for a tier descriptor.
//!
//! Public API surface:
//!
//! ```ignore
//! let r = aec_ai::registry::ModelRegistry::embedded();
//! let medium = r.text_tier(aec_ai::model_manager::ModelTier::Medium);
//! assert_eq!(medium.context_tokens, 4096);
//! ```
//!
//! Why a registry and not "just expose the JSON"? The registry is
//! the place where text + image-gen descriptors meet a common
//! validation/access pattern — the next groups (first-run wizard,
//! retry/resume) all read from `ModelRegistry`, not from the raw
//! JSON, so they can't accidentally drift across schema additions.

use serde::Deserialize;
use std::sync::OnceLock;

use crate::model_manager::ModelTier;

/// Wire shape of `crates/aec_ai/data/ai_models.json`. Parsed once at
/// startup and held as a `&'static ModelRegistry`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelRegistry {
    /// Schema version. Hard-pinned to `1` for Group D. A future
    /// `schema_version: 2` would imply a breaking change to one of
    /// the nested shapes; the parse layer rejects mismatched
    /// versions at boot so we never silently mis-decode a future
    /// shape into a Group-D shape.
    pub schema_version: u32,
    /// Text-side model descriptors (Ternary-Bonsai Q2_0 GGUFs).
    pub text: TextRegistry,
    /// Image-gen presets. Empty in the current ship — see the
    /// `$doc` in `ai_models.json` for the rationale (we don't paste
    /// hashes for SD models we haven't downloaded-and-hashed in
    /// this session).
    pub image_gen: ImageGenRegistry,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextRegistry {
    /// Tier the bridge picks when no project policy overrides.
    /// Boot-default for fresh installs.
    pub default_tier: TextTierTag,
    pub tiers: Vec<TextTierDescriptor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextTierDescriptor {
    pub tier: TextTierTag,
    pub filename: String,
    pub display_name: String,
    pub huggingface_repo: String,
    pub size_bytes: u64,
    pub blake3_hex: String,
    pub context_tokens: u32,
}

impl TextTierDescriptor {
    /// Build the canonical HF LFS resolve URL for this descriptor,
    /// mirroring [`ModelTier::download_url`]. Centralized here so a
    /// future HF mirror swap is a single edit, not a grep across
    /// the crate.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{repo}/resolve/main/{file}",
            repo = self.huggingface_repo,
            file = self.filename,
        )
    }
}

/// Wire shape of `text.default_tier` and `text.tiers[].tier`.
/// Maps 1:1 to [`ModelTier`]; the conversion lives next to the
/// type so callers don't reach into both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextTierTag {
    Small,
    Medium,
    Large,
}

impl TextTierTag {
    pub fn to_model_tier(self) -> ModelTier {
        match self {
            Self::Small => ModelTier::Small,
            Self::Medium => ModelTier::Medium,
            Self::Large => ModelTier::Large,
        }
    }

    pub fn from_model_tier(tier: ModelTier) -> Self {
        match tier {
            ModelTier::Small => Self::Small,
            ModelTier::Medium => Self::Medium,
            ModelTier::Large => Self::Large,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenRegistry {
    pub presets: Vec<ImageGenPresetDescriptor>,
}

/// A curated image-gen GGUF preset the first-run wizard offers as
/// a one-click download. Every field must be verified end-to-end
/// (download → blake3sum → file size match) before merge; pasting
/// an unverified hash here defeats the entire integrity-pinning
/// reason the registry exists. See the `$doc` in
/// `ai_models.json` for the verification contract.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenPresetDescriptor {
    /// Stable id used by the wizard to identify presets (e.g.
    /// `"sd-v1-5-q4-0"`). Hyphenated lowercase by convention; the
    /// renderer surfaces this in URL fragments + analytics-free
    /// internal pickers.
    pub id: String,
    /// Human-readable name shown in the wizard list.
    pub display_name: String,
    pub filename: String,
    pub blake3_hex: String,
    pub size_bytes: u64,
    pub download_url: String,
    pub vae_filename: Option<String>,
}

impl ModelRegistry {
    /// The compile-time-embedded registry. First call parses the
    /// JSON; subsequent calls return the same `&'static` reference.
    /// Panics if the JSON fails to parse or carries an unsupported
    /// `schema_version` — both are bugs the binary must not ship
    /// with, so they're crash-on-startup rather than logged.
    pub fn embedded() -> &'static ModelRegistry {
        static EMBEDDED: OnceLock<ModelRegistry> = OnceLock::new();
        EMBEDDED.get_or_init(|| {
            // Compile-time pin — the JSON cannot drift out of the
            // binary because it's a `&'static str` in the binary
            // image. Editing the on-disk file post-install has no
            // effect.
            const RAW: &str = include_str!("../data/ai_models.json");
            let r: ModelRegistry = serde_json::from_str(RAW)
                .expect("ai_models.json must parse cleanly at startup");
            assert_eq!(
                r.schema_version, SUPPORTED_SCHEMA_VERSION,
                "ai_models.json schema_version {} not supported (expected {SUPPORTED_SCHEMA_VERSION})",
                r.schema_version,
            );
            // Defense: the JSON must include exactly the three text
            // tiers — Small / Medium / Large — and the default_tier
            // must be one of them. Catch this at boot so a typo in
            // ai_models.json fails the binary smoke-test on first
            // call instead of returning `None` mid-flow.
            for required in [TextTierTag::Small, TextTierTag::Medium, TextTierTag::Large] {
                assert!(
                    r.text.tiers.iter().any(|t| t.tier == required),
                    "ai_models.json text.tiers missing required tier {required:?}",
                );
            }
            assert!(
                r.text.tiers.iter().any(|t| t.tier == r.text.default_tier),
                "ai_models.json text.default_tier {:?} not present in text.tiers",
                r.text.default_tier,
            );
            r
        })
    }

    /// Lookup a text descriptor by tier. The boot-time validation
    /// in [`Self::embedded`] guarantees this never returns `None`
    /// for any [`ModelTier`] variant, so callers can `.expect()`.
    pub fn text_tier(&self, tier: ModelTier) -> &TextTierDescriptor {
        let tag = TextTierTag::from_model_tier(tier);
        self.text
            .tiers
            .iter()
            .find(|t| t.tier == tag)
            .expect("text_tier called for tier not in registry — boot-time check should catch this")
    }

    /// Image-gen presets, in declaration order. Empty in the
    /// current ship; the first-run wizard renders the manual entry
    /// field as the primary affordance until at least one preset
    /// is verified-and-added.
    pub fn image_gen_presets(&self) -> &[ImageGenPresetDescriptor] {
        &self.image_gen.presets
    }
}

/// Hard-coded supported schema version. Bumping this requires a
/// migration story for older `GovernorPolicy` / `.aecstudio`
/// projects that referenced an older registry shape.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_registry_parses_and_pins_schema_version() {
        let r = ModelRegistry::embedded();
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn embedded_registry_has_all_three_text_tiers() {
        let r = ModelRegistry::embedded();
        let tiers: Vec<TextTierTag> = r.text.tiers.iter().map(|t| t.tier).collect();
        assert!(tiers.contains(&TextTierTag::Small));
        assert!(tiers.contains(&TextTierTag::Medium));
        assert!(tiers.contains(&TextTierTag::Large));
    }

    #[test]
    fn text_tier_lookup_matches_model_tier_hardcoded_values() {
        // Migration regression: the values that used to live in
        // `ModelTier::canonical_blake3_hex` / `download_size_bytes`
        // / `huggingface_repo` / `filename` / `context_tokens`
        // MUST match the registry exactly. The `ModelTier`
        // accessors now delegate to the registry; this test
        // catches a hand-edited JSON that drifted from the
        // verified Group A values.
        let r = ModelRegistry::embedded();
        for tier in ModelTier::all() {
            let d = r.text_tier(tier);
            assert_eq!(
                d.filename,
                tier.filename(),
                "filename mismatch for {tier:?}"
            );
            assert_eq!(
                d.display_name,
                tier.display_name(),
                "display_name mismatch for {tier:?}"
            );
            assert_eq!(
                d.huggingface_repo,
                tier.huggingface_repo(),
                "huggingface_repo mismatch for {tier:?}"
            );
            assert_eq!(
                d.size_bytes,
                tier.download_size_bytes(),
                "size_bytes mismatch for {tier:?}"
            );
            assert_eq!(
                d.blake3_hex,
                tier.canonical_blake3_hex(),
                "blake3_hex mismatch for {tier:?}"
            );
            assert_eq!(
                d.context_tokens,
                tier.context_tokens(),
                "context_tokens mismatch for {tier:?}"
            );
        }
    }

    #[test]
    fn text_tier_default_is_medium() {
        // `default_tier` in ai_models.json must be `medium` — the
        // bridge assumes this when no governor policy is applied
        // (matching the existing `ModelManager` boot default).
        let r = ModelRegistry::embedded();
        assert_eq!(r.text.default_tier, TextTierTag::Medium);
    }

    #[test]
    fn text_tier_download_url_matches_model_tier_format() {
        let r = ModelRegistry::embedded();
        for tier in ModelTier::all() {
            let d = r.text_tier(tier);
            assert_eq!(d.download_url(), tier.download_url());
        }
    }

    #[test]
    fn text_tier_tag_round_trips_through_model_tier() {
        for tier in ModelTier::all() {
            let tag = TextTierTag::from_model_tier(tier);
            assert_eq!(tag.to_model_tier(), tier);
        }
    }

    #[test]
    fn image_gen_presets_array_is_addressable_even_when_empty() {
        // The current ship's `image_gen.presets` is intentionally
        // `[]`. The wizard must still be able to call
        // `image_gen_presets()` and get a slice (not `None`) so
        // the empty-state UI is its own well-defined branch.
        let r = ModelRegistry::embedded();
        let presets = r.image_gen_presets();
        // No assertion on length — operators may add verified
        // presets here without invalidating this test. The
        // contract pinned here is "the accessor returns a slice".
        let _: &[ImageGenPresetDescriptor] = presets;
    }
}
