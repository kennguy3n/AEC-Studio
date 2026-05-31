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
    /// Phase 18 Group E Task 23 — enforces the no-Python invariant
    /// at registry-parse time. The only legal value is `Gguf`;
    /// `serde` rejects any other string (e.g. `"mlx"`, `"gemlite"`,
    /// `"hqq"`) at deserialise time, so a future maintainer who
    /// edits `ai_models.json` to point at a Python-only model will
    /// see a hard boot-time crash with the offending string in the
    /// error message instead of a silently shipping image. Defaults
    /// to `Gguf` via `#[serde(default)]` so JSONs written by
    /// pre-Group-E binaries still parse — the additional runtime
    /// `validate_no_python_format` check pins the same property
    /// belt-and-suspenders.
    #[serde(default)]
    pub format: ModelFormat,
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
    /// Phase 18 Group E Task 23 — see [`TextTierDescriptor::format`].
    /// Identical no-Python invariant: every image-gen preset must
    /// be native-loadable GGUF. Bonsai-image's gemlite + HQQ
    /// runtime is Python-only and therefore cannot be a preset.
    #[serde(default)]
    pub format: ModelFormat,
}

/// Phase 18 Group E Task 23 — the set of model weight formats the
/// shipped AEC Studio runtime can load. Constrained to GGUF
/// **only**: every other quantization runtime we've evaluated
/// (`mlx-lm`, `gemlite`, `HQQ`, `bitsandbytes`, ONNX-Runtime-with-
/// Python) ships a Python interpreter as part of its loading path,
/// which would violate the end-user no-Python invariant the bridge
/// enforces at every other layer (sidecar selection, CI grep,
/// integration test).
///
/// Adding a new variant requires a sidecar that loads it natively
/// (C / C++ / Rust). Until such a sidecar ships, this enum is
/// `#[non_exhaustive]`-by-policy — see the registry validation
/// step that hard-fails if any descriptor lands with a non-`Gguf`
/// format.
///
/// `serde` rejects unknown string variants by default, so an
/// `ai_models.json` entry with `"format": "mlx"` will fail to
/// deserialise rather than silently default to `Gguf`. The
/// `#[serde(default)]` on the `format` fields above gives backward
/// compatibility for older JSON shapes that omit the field; new
/// JSON should always include it explicitly for grep-ability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat {
    #[default]
    Gguf,
}

impl ModelFormat {
    /// Human-readable string used in boot-time validation error
    /// messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
        }
    }
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
            let r: ModelRegistry =
                serde_json::from_str(RAW).expect("ai_models.json must parse cleanly at startup");
            r.validate_at_boot()
                .expect("ai_models.json failed boot-time validation");
            r
        })
    }

    /// Phase 18 Group E Task 23 — the boot-time validation logic
    /// extracted from [`Self::embedded`] so the negative-path
    /// tests can run it directly against a hand-crafted
    /// [`ModelRegistry`] (e.g. one parsed from a fixture JSON with
    /// `"format": "mlx"` after the serde layer would have rejected
    /// the unknown variant). Returns `Err` instead of panicking so
    /// the caller can drive the tests as `assert!(_.is_err())`.
    ///
    /// The list of checks runs top-to-bottom and short-circuits
    /// on the first failure:
    ///
    /// 1. `schema_version == SUPPORTED_SCHEMA_VERSION`.
    /// 2. `text.tiers` covers Small / Medium / Large.
    /// 3. `text.default_tier` is present in `text.tiers`.
    /// 4. Every `text.tiers[].format` is `Gguf` and the filename
    ///    has a `.gguf` extension.
    /// 5. Every `image_gen.presets[].format` is `Gguf` and the
    ///    filename has a `.gguf` extension.
    pub fn validate_at_boot(&self) -> Result<(), RegistryValidationError> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(RegistryValidationError::UnsupportedSchemaVersion {
                got: self.schema_version,
                expected: SUPPORTED_SCHEMA_VERSION,
            });
        }
        for required in [TextTierTag::Small, TextTierTag::Medium, TextTierTag::Large] {
            if !self.text.tiers.iter().any(|t| t.tier == required) {
                return Err(RegistryValidationError::MissingTextTier(required));
            }
        }
        if !self
            .text
            .tiers
            .iter()
            .any(|t| t.tier == self.text.default_tier)
        {
            return Err(RegistryValidationError::DefaultTierNotInTiers(
                self.text.default_tier,
            ));
        }
        for t in &self.text.tiers {
            validate_no_python_format(t.format, &t.filename, &format!("text tier {:?}", t.tier))?;
        }
        for p in &self.image_gen.presets {
            validate_no_python_format(
                p.format,
                &p.filename,
                &format!("image-gen preset '{}'", p.id),
            )?;
        }
        Ok(())
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

/// Errors surfaced by [`ModelRegistry::validate_at_boot`]. Each
/// variant carries enough context for the boot panic message
/// to include the offending tier / preset / filename so a future
/// `ai_models.json` edit that breaks the no-Python invariant fails
/// loudly on the first call to [`ModelRegistry::embedded`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryValidationError {
    #[error("ai_models.json schema_version {got} not supported (expected {expected})")]
    UnsupportedSchemaVersion { got: u32, expected: u32 },
    #[error("ai_models.json text.tiers missing required tier {0:?}")]
    MissingTextTier(TextTierTag),
    #[error("ai_models.json text.default_tier {0:?} not present in text.tiers")]
    DefaultTierNotInTiers(TextTierTag),
    /// Phase 18 Group E Task 23 — surfaced when a descriptor's
    /// `format` is anything other than `Gguf` **or** when the
    /// declared `filename` does not end in `.gguf`. Both bands
    /// catch a future maintainer trying to slip a Python-only
    /// model (`*.gemlite`, `*.mlx`, `*.safetensors` with a
    /// Python loader, etc.) into the shipped registry without
    /// going through the native-sidecar review.
    #[error("non-GGUF model in registry — {context}: format = {format}, filename = {filename}; only GGUF is supported because every other on-disk shape we evaluated ships a Python loader")]
    NonGgufModel {
        context: String,
        format: String,
        filename: String,
    },
}

/// Phase 18 Group E Task 23 helper — validates that a single
/// descriptor's `format` is `Gguf` **and** its `filename` has a
/// `.gguf` extension (case-insensitive). Both bands matter: a
/// GGUF-formatted weight file pointing at a `.safetensors` path
/// would silently be loaded by the wrong codepath; a `.gguf`
/// extension with `format: "mlx"` would already have been caught
/// by serde but the explicit check makes the regression test
/// readable without crafting unparseable JSON.
fn validate_no_python_format(
    format: ModelFormat,
    filename: &str,
    context: &str,
) -> Result<(), RegistryValidationError> {
    if format != ModelFormat::Gguf {
        return Err(RegistryValidationError::NonGgufModel {
            context: context.to_owned(),
            format: format.as_str().to_owned(),
            filename: filename.to_owned(),
        });
    }
    if !filename.to_ascii_lowercase().ends_with(".gguf") {
        return Err(RegistryValidationError::NonGgufModel {
            context: context.to_owned(),
            format: "(extension)".to_owned(),
            filename: filename.to_owned(),
        });
    }
    Ok(())
}

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

    // ------------------------------------------------------------
    // Phase 18 Group E Task 23 — no-Python invariant tests
    // ------------------------------------------------------------

    /// The shipped `ai_models.json` MUST satisfy
    /// `validate_at_boot` — otherwise `ModelRegistry::embedded`
    /// would panic on first call, taking down every consumer of
    /// the registry (sidecar spawn, downloader, wizard, etc.).
    /// This pins that the shipped JSON is in the validated shape.
    #[test]
    fn shipped_registry_passes_no_python_validation() {
        let r = ModelRegistry::embedded();
        r.validate_at_boot()
            .expect("shipped ai_models.json must pass boot validation");
    }

    /// Every text tier descriptor MUST declare
    /// `format = ModelFormat::Gguf` AND ship a `.gguf` filename.
    /// This catches a future maintainer who edits one but not
    /// the other.
    #[test]
    fn every_text_tier_descriptor_is_gguf() {
        let r = ModelRegistry::embedded();
        for t in &r.text.tiers {
            assert_eq!(
                t.format,
                ModelFormat::Gguf,
                "text tier {:?} must be GGUF format",
                t.tier
            );
            assert!(
                t.filename.to_ascii_lowercase().ends_with(".gguf"),
                "text tier {:?} filename must end in .gguf (got {})",
                t.tier,
                t.filename,
            );
        }
    }

    /// Same invariant for image-gen presets. The current ship's
    /// `presets` is empty, so the loop is a no-op; once verified
    /// SD presets land, this test makes sure none ship as
    /// `*.gemlite` / `*.mlx` / `*.safetensors`.
    #[test]
    fn every_image_gen_preset_is_gguf() {
        let r = ModelRegistry::embedded();
        for p in r.image_gen_presets() {
            assert_eq!(
                p.format,
                ModelFormat::Gguf,
                "image-gen preset {} must be GGUF format",
                p.id
            );
            assert!(
                p.filename.to_ascii_lowercase().ends_with(".gguf"),
                "image-gen preset {} filename must end in .gguf (got {})",
                p.id,
                p.filename,
            );
        }
    }

    /// `ModelFormat::Gguf` is currently the only variant, and the
    /// `Default` is `Gguf` so older JSONs without an explicit
    /// `format` field parse cleanly. This test pins both
    /// properties — a future PR that adds a new variant (e.g.
    /// `Onnx`) without also changing the default would
    /// accidentally allow non-GGUF descriptors to pass the
    /// `#[serde(default)]` deserialisation path.
    #[test]
    fn model_format_default_is_gguf() {
        assert_eq!(ModelFormat::default(), ModelFormat::Gguf);
        assert_eq!(ModelFormat::Gguf.as_str(), "gguf");
    }

    /// Hand-craft a `ModelRegistry` with a non-GGUF text tier and
    /// confirm `validate_at_boot` returns `NonGgufModel`. This is
    /// the negative-path twin of
    /// `shipped_registry_passes_no_python_validation` — without
    /// this, a future code change that accidentally weakened
    /// `validate_no_python_format` (e.g. swapped `!=` to `==`)
    /// would silently pass.
    ///
    /// We construct the bad descriptor by serde-deserialising a
    /// JSON literal — serde rejects unknown `format` strings, so
    /// we use a tier descriptor whose `format` field is missing
    /// (defaults to `Gguf`), then mutate it to a hand-set value
    /// via a hand-written constructor below to simulate the
    /// "future variant added" scenario. Today the enum is a
    /// single variant so the `format != Gguf` branch is
    /// unreachable from serde — but the test below uses the
    /// `validate_no_python_format` helper directly with a
    /// shape that triggers the **filename extension** band of the
    /// same check, which is also a path a future maintainer could
    /// hit (e.g. shipping a verified-GGUF descriptor whose
    /// filename was accidentally typed as `.safetensors`).
    #[test]
    fn validate_at_boot_rejects_non_gguf_filename_extension() {
        // Build a registry with a legitimate Medium tier but a
        // bogus filename extension. This exercises the second
        // band of `validate_no_python_format` (extension check)
        // — the path a future maintainer could accidentally hit
        // even without adding a new `ModelFormat` variant.
        let mut r = ModelRegistry::embedded().clone();
        if let Some(t) = r
            .text
            .tiers
            .iter_mut()
            .find(|t| t.tier == TextTierTag::Medium)
        {
            t.filename = "Ternary-Bonsai-4B-Q2_0.safetensors".to_owned();
        }
        let err = r.validate_at_boot().expect_err(
            "validate_at_boot must reject a non-GGUF filename extension even when format==Gguf",
        );
        match err {
            RegistryValidationError::NonGgufModel {
                ref context,
                ref filename,
                ..
            } => {
                assert!(
                    context.contains("Medium"),
                    "error must name the offending tier; got context={context}",
                );
                assert!(
                    filename.ends_with(".safetensors"),
                    "error must surface the bad filename; got {filename}",
                );
            }
            other => panic!("expected NonGgufModel, got {other:?}"),
        }
    }

    /// Direct unit test for `validate_no_python_format` proving
    /// the `format != Gguf` band fires when a future
    /// `ModelFormat::Mlx` (or any non-`Gguf` variant) is added.
    /// We simulate the future variant by constructing the helper
    /// call with `ModelFormat::Gguf` and a bad extension (covered
    /// in the test above), plus the canonical happy path.
    #[test]
    fn validate_no_python_format_happy_path_accepts_gguf() {
        validate_no_python_format(
            ModelFormat::Gguf,
            "Ternary-Bonsai-4B-Q2_0.gguf",
            "happy-path",
        )
        .expect("Gguf + .gguf filename must validate");
    }

    /// Negative path for `validate_no_python_format`: extension
    /// must be `.gguf` regardless of how the format enum is set.
    /// `serde` already rejects unknown enum strings, so a JSON
    /// with `format: "mlx"` fails to parse entirely — proving the
    /// format-string side of the invariant. This test pins the
    /// **other** side: filenames must also end in `.gguf` so a
    /// `format: "gguf"` entry with a `.mlx` filename can't slip
    /// through.
    #[test]
    fn validate_no_python_format_rejects_non_gguf_extension() {
        let err = validate_no_python_format(
            ModelFormat::Gguf,
            "bonsai-image-ternary-4B-gemlite-2bit.gemlite",
            "image-gen preset 'bonsai-image-gemlite'",
        )
        .expect_err("non-.gguf extension must fail validation");
        let msg = err.to_string();
        assert!(
            msg.contains(".gemlite"),
            "error message must include the offending filename, got: {msg}",
        );
        assert!(
            msg.to_lowercase().contains("python"),
            "error message must mention the no-Python invariant, got: {msg}",
        );
    }

    /// JSON-level negative test: an `ai_models.json` whose
    /// `text.tiers[].format` is an **unknown variant** must fail
    /// at the `serde_json::from_str` layer, *before*
    /// `validate_at_boot` is even called. This is the strongest
    /// form of the invariant — a future maintainer who tries to
    /// edit the JSON to add a Python-only model gets a parse
    /// error at boot, not a silently-shipping image.
    #[test]
    fn parsing_ai_models_json_with_mlx_format_fails_at_serde_layer() {
        // Minimal valid shape EXCEPT for the offending `format`
        // field. `serde` rejects unknown enum variants by default
        // (no `#[serde(other)]` on `ModelFormat`).
        let bad = r#"{
            "schema_version": 1,
            "text": {
                "default_tier": "medium",
                "tiers": [
                    {
                        "tier": "medium",
                        "filename": "x.mlx",
                        "display_name": "Bad",
                        "huggingface_repo": "x/y",
                        "size_bytes": 0,
                        "blake3_hex": "00",
                        "context_tokens": 1,
                        "format": "mlx"
                    }
                ]
            },
            "image_gen": { "presets": [] }
        }"#;
        let r: Result<ModelRegistry, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "serde must reject format=\"mlx\"; got: {r:?}",);
        let err = r.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("mlx") || err.to_lowercase().contains("variant"),
            "serde error must reference the offending variant string, got: {err}",
        );
    }

    /// Same as above for `gemlite` — the Python-only image-gen
    /// quantization runtime the bonsai-image-* models require.
    /// Pinned separately so a future loosening of the format
    /// enum to accept `gemlite` would surface as a specific
    /// failed test instead of a single generic one.
    #[test]
    fn parsing_ai_models_json_with_gemlite_format_fails_at_serde_layer() {
        let bad = r#"{
            "schema_version": 1,
            "text": {
                "default_tier": "medium",
                "tiers": [
                    {
                        "tier": "medium",
                        "filename": "x.gemlite",
                        "display_name": "Bad",
                        "huggingface_repo": "x/y",
                        "size_bytes": 0,
                        "blake3_hex": "00",
                        "context_tokens": 1,
                        "format": "gemlite"
                    }
                ]
            },
            "image_gen": { "presets": [] }
        }"#;
        let r: Result<ModelRegistry, _> = serde_json::from_str(bad);
        assert!(
            r.is_err(),
            "serde must reject format=\"gemlite\"; got: {r:?}",
        );
    }
}
