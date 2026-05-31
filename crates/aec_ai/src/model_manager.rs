//! Ternary-Bonsai 1.58-bit GGUF model selection and lifecycle management.
//!
//! Selects the appropriate model size (1.7B / 4B / 8B) based on the host's
//! hardware profile (total RAM + GPU VRAM), downloads the model file on
//! first use from the prism-ml HuggingFace repos, and verifies its BLAKE3
//! checksum.
//!
//! ## Models
//!
//! | Tier   | Family             | Quant      | Format | Size     |
//! |--------|--------------------|------------|--------|---------:|
//! | Small  | Ternary-Bonsai 1.7B | 1.58-bit  | GGUF Q2_0 |  442 MiB |
//! | Medium | Ternary-Bonsai 4B   | 1.58-bit  | GGUF Q2_0 | 1.00 GiB |
//! | Large  | Ternary-Bonsai 8B   | 1.58-bit  | GGUF Q2_0 | 2.03 GiB |
//!
//! GGUF Q2_0 (g128) packs ternary `{-1, 0, +1}` weights at an effective
//! 2.125 bits/weight (1.58 bits of information per weight + one FP16
//! group-wise scale per 128 weights). The fourth 2-bit code point is
//! reserved by the PrismML `llama.cpp` fork for future extensions and is
//! unused for ternary weights, so Q2_0 is effectively lossless for the
//! Ternary-Bonsai weight set.
//!
//! ## Lifecycle
//!
//! The manager is stateless across sessions — it reads the models directory
//! on startup, checks what's available, and downloads what's missing.
//! Tier switching at runtime is supported by updating the `RuntimeConfig`
//! returned by [`ModelManager::active_config`] and restarting the sidecar.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model_download::{self, DownloadError, ProgressCallback};
use crate::runtime::RuntimeConfig;

#[derive(Debug, Error)]
pub enum ModelManagerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("no suitable model tier for this hardware (RAM: {ram_mb} MB, VRAM: {vram_mb} MB)")]
    NoSuitableTier { ram_mb: u64, vram_mb: u32 },
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("no descriptor registered for tier {tier:?}")]
    MissingDescriptor { tier: ModelTier },
    #[error("descriptor for tier {tier:?} has no download_url")]
    NoDownloadUrl { tier: ModelTier },
    #[error("download failed: {0}")]
    Download(String),
}

impl From<DownloadError> for ModelManagerError {
    fn from(value: DownloadError) -> Self {
        Self::Download(value.to_string())
    }
}

/// Ternary-Bonsai model size tiers. Each tier maps to a single GGUF Q2_0
/// file in the corresponding `prism-ml/Ternary-Bonsai-*-gguf` HuggingFace
/// repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Ternary-Bonsai 1.7B (Q2_0). 442 MiB on disk, ~1.5 GiB resident.
    /// Runs on ≥ 4 GB RAM, no GPU required.
    Small,
    /// Ternary-Bonsai 4B (Q2_0). 1.00 GiB on disk, ~2.5 GiB resident.
    /// Needs ≥ 8 GB RAM or ≥ 4 GB VRAM.
    Medium,
    /// Ternary-Bonsai 8B (Q2_0). 2.03 GiB on disk, ~4.5 GiB resident.
    /// Needs ≥ 16 GB RAM or ≥ 8 GB VRAM.
    Large,
}

impl ModelTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Small => "small_1.7b",
            Self::Medium => "medium_4b",
            Self::Large => "large_8b",
        }
    }

    /// Lowercase tier slug used by the IPC / Settings UI layer.
    /// `"small" | "medium" | "large"`.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// Inverse of [`ModelTier::slug`]. Accepts the bare lowercase form
    /// only — for the long `"small_1.7b"` form, parse via [`Self::as_str`]
    /// callers in the same code path.
    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    /// Canonical GGUF Q2_0 filename used by the prism-ml HF repos.
    ///
    /// Reads from the compile-time-embedded
    /// [`crate::registry::ModelRegistry`] (single source of truth
    /// as of Phase 18 Group D Task 18). The registry boot-time
    /// validation guarantees every tier is present, so the
    /// `expect` here is unreachable in practice.
    pub fn filename(&self) -> &'static str {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .filename
            .as_str()
    }

    /// Human-readable name for the model, used in Settings UI and download
    /// dialogs. Includes the family, parameter count, and quantization.
    pub fn display_name(&self) -> &'static str {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .display_name
            .as_str()
    }

    /// Sidecar context window size for this tier. Ternary-Bonsai models
    /// natively support 32 768 tokens, but we cap each tier to a sensible
    /// RAM-budget value (`llama-server --ctx-size`). Larger contexts
    /// allocate more KV cache, so this is the hardware-tier cap, not the
    /// model's intrinsic maximum.
    pub fn context_tokens(&self) -> u32 {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .context_tokens
    }

    /// On-disk Q2_0 GGUF size in bytes (matches the HuggingFace LFS
    /// pointer). Used by the download progress UI to render the total
    /// before the response Content-Length is known.
    pub fn download_size_bytes(&self) -> u64 {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .size_bytes
    }

    /// Canonical BLAKE3 checksum of the Q2_0 GGUF as published by
    /// prism-ml. Computed by downloading each file from
    /// `https://huggingface.co/prism-ml/Ternary-Bonsai-*-gguf/resolve/main/*.gguf`
    /// and hashing with BLAKE3.
    pub fn canonical_blake3_hex(&self) -> &'static str {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .blake3_hex
            .as_str()
    }

    /// HuggingFace repo slug for this tier (used to build the `resolve/main`
    /// download URL).
    pub fn huggingface_repo(&self) -> &'static str {
        crate::registry::ModelRegistry::embedded()
            .text_tier(*self)
            .huggingface_repo
            .as_str()
    }

    /// Direct-download URL on the HuggingFace CDN for this tier's GGUF.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{repo}/resolve/main/{file}",
            repo = self.huggingface_repo(),
            file = self.filename(),
        )
    }

    /// Default descriptor used by [`ModelManager::default_descriptors`].
    /// All values come from the model card published on HuggingFace and
    /// from this codebase's own verification (BLAKE3).
    pub fn default_descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            tier: *self,
            filename: self.filename().to_string(),
            blake3_hex: self.canonical_blake3_hex().to_string(),
            download_url: Some(self.download_url()),
            size_bytes: self.download_size_bytes(),
        }
    }

    /// All tiers, in increasing-size order.
    pub fn all() -> [ModelTier; 3] {
        [Self::Small, Self::Medium, Self::Large]
    }
}

/// Descriptor for a model file, including its expected BLAKE3 checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub tier: ModelTier,
    pub filename: String,
    pub blake3_hex: String,
    pub download_url: Option<String>,
    pub size_bytes: u64,
}

/// Manages model files on disk. Selects the best tier for the hardware,
/// verifies checksums, and provides a `RuntimeConfig` pointing at the
/// active model.
#[derive(Debug)]
pub struct ModelManager {
    models_dir: PathBuf,
    descriptors: Vec<ModelDescriptor>,
    active_tier: ModelTier,
}

/// Lifecycle states a download passes through. Emitted via
/// [`DownloadCallbacks::on_state`] so callers (e.g. the bridge
/// service's progress slot) can publish UI-visible status transitions
/// without re-implementing the download / verify / rename sequence.
///
/// On a fresh, successful download the order is:
/// `Downloading` → `Verifying` → `Completed`. On a fast-path (file
/// already present with correct BLAKE3) only `Completed` fires. On
/// any failure the last state is `Failed { stage, msg }`.
#[derive(Debug, Clone)]
pub enum DownloadState {
    /// HTTPS transfer in flight. Followed by `Verifying` once the
    /// transfer completes.
    Downloading,
    /// All bytes are on disk and BLAKE3 hashing has started.
    Verifying,
    /// File is verified and renamed into the final path.
    Completed,
    /// Terminal failure. `stage` is one of `"download"`, `"verify"`,
    /// `"rename"` so the renderer can build a useful error message
    /// without parsing `msg`.
    Failed { stage: &'static str, msg: String },
}

/// Bundle of optional callbacks for [`ModelManager::download_model`].
///
/// * `on_progress` — invoked on every ~1 MiB chunk during the HTTPS
///   transfer with `(downloaded_bytes, total_bytes)`.
/// * `on_state` — invoked on each [`DownloadState`] transition.
///
/// `DownloadCallbacks::default()` opts out of both — useful for CLI
/// tools or tests that just want a blocking download.
#[derive(Default, Clone)]
pub struct DownloadCallbacks {
    pub on_progress: Option<ProgressCallback>,
    pub on_state: Option<Arc<dyn Fn(DownloadState) + Send + Sync>>,
}

impl DownloadCallbacks {
    fn fire_state(&self, s: DownloadState) {
        if let Some(f) = &self.on_state {
            f(s);
        }
    }
}

impl ModelManager {
    /// Create a manager rooted at `models_dir`. The `descriptors` list
    /// defines the known model files and their expected checksums.
    pub fn new(
        models_dir: PathBuf,
        descriptors: Vec<ModelDescriptor>,
        total_ram_mb: u64,
        vram_mb: u32,
    ) -> Result<Self, ModelManagerError> {
        let tier = select_tier(total_ram_mb, vram_mb)?;
        Ok(Self {
            models_dir,
            descriptors,
            active_tier: tier,
        })
    }

    /// Convenience constructor that seeds the descriptor list with the
    /// canonical Ternary-Bonsai entries from
    /// [`ModelTier::default_descriptor`]. Used by the bridge service
    /// where the descriptor list never needs to be supplied externally.
    pub fn with_default_descriptors(
        models_dir: PathBuf,
        total_ram_mb: u64,
        vram_mb: u32,
    ) -> Result<Self, ModelManagerError> {
        Self::new(
            models_dir,
            ModelTier::all()
                .iter()
                .map(ModelTier::default_descriptor)
                .collect(),
            total_ram_mb,
            vram_mb,
        )
    }

    /// Construct a manager with an explicit active tier. The bridge
    /// service uses this at boot before the governor's hardware probe
    /// is available, then promotes via [`Self::set_tier`] once the
    /// renderer surfaces the governor / user choice.
    pub fn with_tier(
        models_dir: PathBuf,
        descriptors: Vec<ModelDescriptor>,
        active_tier: ModelTier,
    ) -> Self {
        Self {
            models_dir,
            descriptors,
            active_tier,
        }
    }

    /// Built-in default descriptor list — the three Ternary-Bonsai tiers
    /// with their canonical filenames, sizes, and checksums.
    pub fn default_descriptors() -> Vec<ModelDescriptor> {
        ModelTier::all()
            .iter()
            .map(ModelTier::default_descriptor)
            .collect()
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn descriptors(&self) -> &[ModelDescriptor] {
        &self.descriptors
    }

    pub fn descriptor(&self, tier: ModelTier) -> Option<&ModelDescriptor> {
        self.descriptors.iter().find(|d| d.tier == tier)
    }

    pub fn active_tier(&self) -> ModelTier {
        self.active_tier
    }

    /// Switch to a different tier at runtime (e.g. from Settings).
    pub fn set_tier(&mut self, tier: ModelTier) {
        self.active_tier = tier;
    }

    /// Path the file *for `tier`* should be at on disk.
    pub fn model_path_for(&self, tier: ModelTier) -> PathBuf {
        self.models_dir.join(tier.filename())
    }

    /// Path to the active model file.
    pub fn active_model_path(&self) -> PathBuf {
        self.model_path_for(self.active_tier)
    }

    /// Check whether the active model file exists on disk.
    pub fn is_active_model_available(&self) -> bool {
        self.active_model_path().is_file()
    }

    /// Check whether the given tier's model file exists on disk.
    pub fn is_model_available(&self, tier: ModelTier) -> bool {
        self.model_path_for(tier).is_file()
    }

    /// Build a `RuntimeConfig` for the currently active tier.
    pub fn active_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            model_path: self.active_model_path(),
            max_context_tokens: self.active_tier.context_tokens(),
            ..RuntimeConfig::default()
        }
    }

    /// Verify the BLAKE3 checksum of the active model file.
    pub fn verify_active_checksum(&self) -> Result<bool, ModelManagerError> {
        self.verify_checksum(self.active_tier)
    }

    /// Verify the BLAKE3 checksum of the given tier's model file.
    pub fn verify_checksum(&self, tier: ModelTier) -> Result<bool, ModelManagerError> {
        let path = self.model_path_for(tier);
        let descriptor = self.descriptor(tier);
        let Some(desc) = descriptor else {
            return Ok(true); // No descriptor → skip verification.
        };
        Self::verify_file(&path, &desc.blake3_hex)
    }

    /// Verify a file's BLAKE3 against an expected hex digest. Returns
    /// `Ok(true)` on match, `Ok(false)` only when the expected digest
    /// is the empty string (no checksum supplied), and
    /// [`ModelManagerError::ModelNotFound`] /
    /// [`ModelManagerError::ChecksumMismatch`] otherwise. Bridge
    /// callers that want a bool-on-mismatch (rather than an error)
    /// match on the `ChecksumMismatch` variant.
    pub fn verify_file(path: &Path, expected_hex: &str) -> Result<bool, ModelManagerError> {
        if !path.is_file() {
            return Err(ModelManagerError::ModelNotFound(path.display().to_string()));
        }
        if expected_hex.is_empty() {
            return Ok(false);
        }
        let actual = blake3_file(path)?;
        if actual != expected_hex {
            return Err(ModelManagerError::ChecksumMismatch {
                path: path.display().to_string(),
                expected: expected_hex.to_string(),
                actual,
            });
        }
        Ok(true)
    }

    /// Write model bytes to the models directory for the given tier,
    /// verifying the BLAKE3 checksum. Used by the in-process install path
    /// (e.g. tests that bundle the bytes inline).
    pub fn install_model(
        &self,
        tier: ModelTier,
        data: &[u8],
        expected_blake3: &str,
    ) -> Result<PathBuf, ModelManagerError> {
        let actual = hex::encode(blake3::hash(data).as_bytes());
        if actual != expected_blake3 {
            return Err(ModelManagerError::ChecksumMismatch {
                path: tier.filename().into(),
                expected: expected_blake3.into(),
                actual,
            });
        }
        std::fs::create_dir_all(&self.models_dir)?;
        let path = self.model_path_for(tier);
        let mut file = std::fs::File::create(&path)?;
        file.write_all(data)?;
        file.flush()?;
        Ok(path)
    }

    /// Download the model file for `tier` from its descriptor's
    /// `download_url` to the models directory. Verifies BLAKE3 on
    /// completion and atomically renames the partial file into place.
    ///
    /// The download is **resumable**: if a `<filename>.partial` file
    /// already exists from a previous attempt, a `Range: bytes=N-` header
    /// is sent and the new bytes are appended.
    ///
    /// **Recovery fast path**: if `<filename>.partial` already hashes
    /// to the expected BLAKE3 (i.e. a previous attempt finished the
    /// transfer but crashed before the atomic rename), the HTTPS
    /// transfer is skipped entirely and the partial is renamed into
    /// place. Without this, a resume request on a complete file would
    /// send `Range: bytes=<total>-` and receive HTTP 416, leaving the
    /// caller permanently stuck (see the layer-2 416 deletion in
    /// [`model_download::download_to_file`] for the defence-in-depth
    /// companion fix).
    ///
    /// `callbacks.on_progress` is called periodically during the
    /// download with `(downloaded_bytes, total_bytes)`. `callbacks.on_state`
    /// is called on each lifecycle transition
    /// ([`DownloadState`]); pass `DownloadCallbacks::default()` to
    /// disable both.
    ///
    /// Returns the absolute path to the verified model file.
    pub fn download_model(
        &self,
        tier: ModelTier,
        callbacks: DownloadCallbacks,
    ) -> Result<PathBuf, ModelManagerError> {
        let descriptor = self
            .descriptor(tier)
            .ok_or(ModelManagerError::MissingDescriptor { tier })?
            .clone();
        let Some(url) = descriptor.download_url.clone() else {
            return Err(ModelManagerError::NoDownloadUrl { tier });
        };
        std::fs::create_dir_all(&self.models_dir)?;
        let final_path = self.model_path_for(tier);
        // Fast path: already on disk and verified.
        if final_path.is_file() {
            let actual = blake3_file(&final_path)?;
            if actual == descriptor.blake3_hex {
                callbacks.fire_state(DownloadState::Completed);
                return Ok(final_path);
            }
            // File exists but checksum is wrong — delete and re-download.
            std::fs::remove_file(&final_path)?;
        }
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", descriptor.filename));
        // Fast path: a previous attempt may have completed the download
        // (correct BLAKE3) but crashed between the verify step below and
        // the atomic rename. Without this check, the next call would
        // resume from the end of the file, send `Range: bytes=<total>-`,
        // get HTTP 416 and treat that as a fatal error — leaving the
        // partial file in place so every subsequent retry hits the same
        // 416 (permanent failure). Hash the partial first; if it
        // matches, skip the HTTP transfer entirely and proceed to
        // rename. The BLAKE3 hash is the authoritative completeness
        // check, not the file size.
        if partial_path.is_file() {
            if let Ok(actual) = blake3_file(&partial_path) {
                if actual == descriptor.blake3_hex {
                    callbacks.fire_state(DownloadState::Verifying);
                    if let Err(e) = std::fs::rename(&partial_path, &final_path) {
                        callbacks.fire_state(DownloadState::Failed {
                            stage: "rename",
                            msg: e.to_string(),
                        });
                        return Err(e.into());
                    }
                    callbacks.fire_state(DownloadState::Completed);
                    return Ok(final_path);
                }
            }
        }
        callbacks.fire_state(DownloadState::Downloading);
        if let Err(e) = model_download::download_to_file(
            &url,
            &partial_path,
            descriptor.size_bytes,
            callbacks.on_progress.clone(),
        ) {
            callbacks.fire_state(DownloadState::Failed {
                stage: "download",
                msg: e.to_string(),
            });
            return Err(e.into());
        }
        // Verify BLAKE3 of the partial file before renaming.
        callbacks.fire_state(DownloadState::Verifying);
        let actual = match blake3_file(&partial_path) {
            Ok(h) => h,
            Err(e) => {
                callbacks.fire_state(DownloadState::Failed {
                    stage: "verify",
                    msg: e.to_string(),
                });
                return Err(e);
            }
        };
        if actual != descriptor.blake3_hex {
            // Bad checksum: delete the partial so the next attempt starts
            // from scratch rather than resuming a corrupt file.
            let _ = std::fs::remove_file(&partial_path);
            let err = ModelManagerError::ChecksumMismatch {
                path: partial_path.display().to_string(),
                expected: descriptor.blake3_hex.clone(),
                actual,
            };
            callbacks.fire_state(DownloadState::Failed {
                stage: "verify",
                msg: err.to_string(),
            });
            return Err(err);
        }
        // Atomic rename: on every platform std::fs::rename is atomic when
        // both paths are on the same filesystem (which they are here —
        // both inside models_dir).
        if let Err(e) = std::fs::rename(&partial_path, &final_path) {
            callbacks.fire_state(DownloadState::Failed {
                stage: "rename",
                msg: e.to_string(),
            });
            return Err(e.into());
        }
        callbacks.fire_state(DownloadState::Completed);
        Ok(final_path)
    }

    /// Delete a non-active tier's model file. Returns an error if the
    /// caller tries to delete the active tier's file.
    pub fn delete_model(&self, tier: ModelTier) -> Result<(), ModelManagerError> {
        if tier == self.active_tier {
            return Err(ModelManagerError::Download(format!(
                "cannot delete active tier {tier:?}",
            )));
        }
        let path = self.model_path_for(tier);
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        // Also clean up any partial file left from a failed download.
        let partial = self.models_dir.join(format!("{}.partial", tier.filename()));
        if partial.is_file() {
            std::fs::remove_file(&partial)?;
        }
        Ok(())
    }

    /// Total bytes occupied by every downloaded model file under
    /// `models_dir` (active + non-active). Includes `.partial` files
    /// so a paused download is still accounted for.
    pub fn disk_usage(&self) -> Result<u64, ModelManagerError> {
        let mut total = 0u64;
        for tier in ModelTier::all() {
            let path = self.model_path_for(tier);
            if let Ok(meta) = std::fs::metadata(&path) {
                total = total.saturating_add(meta.len());
            }
            let partial = self.models_dir.join(format!("{}.partial", tier.filename()));
            if let Ok(meta) = std::fs::metadata(&partial) {
                total = total.saturating_add(meta.len());
            }
        }
        Ok(total)
    }

    /// List all available (on-disk) models.
    pub fn available_models(&self) -> Vec<(ModelTier, PathBuf)> {
        ModelTier::all()
            .into_iter()
            .filter_map(|t| {
                let path = self.model_path_for(t);
                path.is_file().then_some((t, path))
            })
            .collect()
    }
}

/// Convenience: build a `ProgressCallback` that forwards to a closure.
/// Useful in callsites that already have an `Arc<...>` to forward into.
pub fn progress_arc<F>(f: F) -> ProgressCallback
where
    F: Fn(u64, u64) + Send + Sync + 'static,
{
    Arc::new(f)
}

/// Select the best model tier based on hardware. Prefers the largest tier
/// the hardware can comfortably support.
///
/// Heuristic (RAM thresholds drop sharply relative to legacy Q4_K_M
/// because Ternary-Bonsai Q2_0 is roughly 4× smaller per parameter):
///   - VRAM ≥ 8 GB or RAM ≥ 16 GB → Large (8B, 2 GB on disk)
///   - VRAM ≥ 4 GB or RAM ≥  8 GB → Medium (4B, 1 GB on disk)
///   - RAM ≥ 4 GB                  → Small (1.7B, 442 MB on disk)
///   - Otherwise → error
pub fn select_tier(total_ram_mb: u64, vram_mb: u32) -> Result<ModelTier, ModelManagerError> {
    if vram_mb >= 8192 || total_ram_mb >= 16384 {
        return Ok(ModelTier::Large);
    }
    if vram_mb >= 4096 || total_ram_mb >= 8192 {
        return Ok(ModelTier::Medium);
    }
    if total_ram_mb >= 4096 {
        return Ok(ModelTier::Small);
    }
    Err(ModelManagerError::NoSuitableTier {
        ram_mb: total_ram_mb,
        vram_mb,
    })
}

/// Compute the BLAKE3 hash of a file on disk, reading in 64 KiB chunks.
fn blake3_file(path: &Path) -> Result<String, ModelManagerError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn select_tier_large_with_high_vram() {
        assert_eq!(select_tier(4096, 8192).unwrap(), ModelTier::Large);
    }

    #[test]
    fn select_tier_large_with_high_ram() {
        assert_eq!(select_tier(16384, 0).unwrap(), ModelTier::Large);
    }

    #[test]
    fn select_tier_medium_with_moderate_hardware() {
        assert_eq!(select_tier(8192, 0).unwrap(), ModelTier::Medium);
        assert_eq!(select_tier(4096, 4096).unwrap(), ModelTier::Medium);
    }

    #[test]
    fn select_tier_small_with_low_hardware() {
        assert_eq!(select_tier(4096, 0).unwrap(), ModelTier::Small);
    }

    #[test]
    fn select_tier_fails_below_minimum() {
        assert!(select_tier(2048, 0).is_err());
    }

    #[test]
    fn model_tier_filenames_match_huggingface_repos() {
        assert_eq!(ModelTier::Small.filename(), "Ternary-Bonsai-1.7B-Q2_0.gguf");
        assert_eq!(ModelTier::Medium.filename(), "Ternary-Bonsai-4B-Q2_0.gguf");
        assert_eq!(ModelTier::Large.filename(), "Ternary-Bonsai-8B-Q2_0.gguf");
    }

    #[test]
    fn model_tier_slug_round_trips() {
        for tier in ModelTier::all() {
            assert_eq!(ModelTier::from_slug(tier.slug()), Some(tier));
        }
        assert_eq!(ModelTier::from_slug("SMALL"), None);
        assert_eq!(ModelTier::from_slug(""), None);
        assert_eq!(ModelTier::from_slug("tiny"), None);
        assert_eq!(ModelTier::from_slug("small_1.7b"), None);
    }

    #[test]
    fn model_tier_display_names_mention_ternary_quantization() {
        for tier in ModelTier::all() {
            let name = tier.display_name();
            assert!(
                name.contains("Ternary-Bonsai"),
                "{name} should name the family"
            );
            assert!(name.contains("1.58-bit"), "{name} should mention 1.58-bit");
            assert!(
                name.contains("GGUF Q2_0"),
                "{name} should mention GGUF Q2_0"
            );
        }
    }

    #[test]
    fn download_urls_target_huggingface_resolve_endpoint() {
        for tier in ModelTier::all() {
            let url = tier.download_url();
            assert!(url.starts_with("https://huggingface.co/prism-ml/"));
            assert!(url.contains("/resolve/main/"));
            assert!(url.ends_with(tier.filename()));
        }
    }

    #[test]
    fn default_descriptors_populate_all_three_tiers() {
        let descs = ModelManager::default_descriptors();
        assert_eq!(descs.len(), 3);
        let tiers: Vec<_> = descs.iter().map(|d| d.tier).collect();
        assert_eq!(
            tiers,
            vec![ModelTier::Small, ModelTier::Medium, ModelTier::Large]
        );
        for desc in &descs {
            assert!(
                !desc.blake3_hex.is_empty(),
                "tier {:?} missing checksum",
                desc.tier
            );
            assert_eq!(desc.blake3_hex.len(), 64, "BLAKE3 hex is 64 chars");
            assert!(
                desc.download_url.is_some(),
                "tier {:?} missing url",
                desc.tier
            );
            assert!(desc.size_bytes > 0, "tier {:?} missing size", desc.tier);
        }
    }

    #[test]
    fn install_model_verifies_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        let data = b"fake model data for testing";
        let hash = hex::encode(blake3::hash(data).as_bytes());
        let path = mgr.install_model(ModelTier::Small, data, &hash).unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }

    #[test]
    fn install_model_rejects_bad_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        let result = mgr.install_model(ModelTier::Small, b"data", "bad_hash");
        assert!(matches!(
            result,
            Err(ModelManagerError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn active_config_uses_selected_tier() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        assert_eq!(mgr.active_tier(), ModelTier::Large);
        mgr.set_tier(ModelTier::Small);
        assert_eq!(mgr.active_tier(), ModelTier::Small);
        let cfg = mgr.active_config();
        assert!(cfg
            .model_path
            .to_string_lossy()
            .contains("Ternary-Bonsai-1.7B-Q2_0.gguf"));
        assert_eq!(cfg.max_context_tokens, 2048);
    }

    #[test]
    fn blake3_file_computes_correct_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let hash = blake3_file(&path).unwrap();
        let expected = hex::encode(blake3::hash(b"hello world").as_bytes());
        assert_eq!(hash, expected);
    }

    #[test]
    fn available_models_lists_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ModelTier::Small.filename()), b"x").unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 4096, 0).unwrap();
        let available = mgr.available_models();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].0, ModelTier::Small);
    }

    #[test]
    fn disk_usage_sums_all_tiers_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(ModelTier::Small.filename()),
            vec![0u8; 1024],
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join(format!("{}.partial", ModelTier::Medium.filename())),
            vec![0u8; 512],
        )
        .unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        assert_eq!(mgr.disk_usage().unwrap(), 1024 + 512);
    }

    #[test]
    fn delete_model_refuses_active_tier() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        mgr.set_tier(ModelTier::Small);
        let err = mgr.delete_model(ModelTier::Small).unwrap_err();
        assert!(matches!(err, ModelManagerError::Download(_)));
    }

    #[test]
    fn delete_model_removes_non_active_file_and_partial() {
        let dir = tempfile::tempdir().unwrap();
        let small_path = dir.path().join(ModelTier::Small.filename());
        std::fs::write(&small_path, b"x").unwrap();
        let partial_path = dir
            .path()
            .join(format!("{}.partial", ModelTier::Small.filename()));
        std::fs::write(&partial_path, b"y").unwrap();
        let mut mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        mgr.set_tier(ModelTier::Large);
        mgr.delete_model(ModelTier::Small).unwrap();
        assert!(!small_path.exists());
        assert!(!partial_path.exists());
    }

    #[test]
    fn verify_active_checksum_with_no_descriptor_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        // No descriptor registered → skip verification.
        assert!(mgr.verify_active_checksum().unwrap());
    }

    /// Reproduces the 416-permanent-failure bug: a previous attempt
    /// fully downloaded the model into `<filename>.partial`, BLAKE3
    /// matched, but the process died between BLAKE3 verification and
    /// the atomic rename. Calling `download_model` again must NOT issue
    /// any HTTP request (the partial is already correct); it must
    /// detect the complete partial by hash and rename it into place.
    ///
    /// We force "must not issue HTTP" by giving the descriptor a
    /// non-allowlisted `download_url` — if the fast path were missing,
    /// `model_download::download_to_file` would reject the URL with
    /// `HostNotAllowed` and the test would fail.
    #[test]
    fn download_model_skips_http_when_partial_already_matches_blake3() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"complete-but-not-yet-renamed payload bytes";
        let filename = ModelTier::Small.filename().to_string();
        let partial_path = dir.path().join(format!("{filename}.partial"));
        std::fs::write(&partial_path, payload).unwrap();
        let expected_hash = hex::encode(blake3::hash(payload).as_bytes());

        let descriptor = ModelDescriptor {
            tier: ModelTier::Small,
            filename: filename.clone(),
            blake3_hex: expected_hash.clone(),
            // Intentionally NOT on the HuggingFace allow-list — if the
            // fast path is missing, download_to_file rejects this with
            // HostNotAllowed and the test fails.
            download_url: Some("https://example.invalid/should-not-be-called".to_string()),
            size_bytes: payload.len() as u64,
        };
        let mgr =
            ModelManager::new(dir.path().to_path_buf(), vec![descriptor], 16384, 8192).unwrap();

        let states: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let states_for_cb = states.clone();
        let callbacks = DownloadCallbacks {
            on_progress: None,
            on_state: Some(Arc::new(move |s| {
                let label = match s {
                    DownloadState::Downloading => "downloading",
                    DownloadState::Verifying => "verifying",
                    DownloadState::Completed => "completed",
                    DownloadState::Failed { .. } => "failed",
                };
                states_for_cb.lock().unwrap().push(label);
            })),
        };

        let final_path = mgr.download_model(ModelTier::Small, callbacks).unwrap();

        assert_eq!(final_path, dir.path().join(&filename));
        assert!(final_path.is_file(), "final file must exist after rename");
        assert!(
            !partial_path.exists(),
            "partial file must be renamed away, not left in place"
        );
        let observed = states.lock().unwrap().clone();
        assert!(
            !observed.contains(&"downloading"),
            "fast path must not emit Downloading (no HTTP should occur); observed: {observed:?}"
        );
        assert!(
            observed.contains(&"completed"),
            "fast path must terminate in Completed; observed: {observed:?}"
        );
    }

    /// If a `.partial` file exists but has the wrong BLAKE3 (e.g.
    /// corrupted mid-download by the previous attempt), the fast path
    /// must NOT take it. We don't test the full download flow here (no
    /// network in unit tests); we just confirm the fast path is hash-
    /// gated. With a wrong hash and a non-allowlisted URL,
    /// `download_to_file` is reached and rejects with `HostNotAllowed`,
    /// which propagates as an error — proving the fast path was
    /// bypassed.
    #[test]
    fn download_model_does_not_skip_http_when_partial_blake3_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let filename = ModelTier::Small.filename().to_string();
        let partial_path = dir.path().join(format!("{filename}.partial"));
        std::fs::write(&partial_path, b"corrupt-bytes").unwrap();
        // descriptor's expected hash is for *different* content.
        let expected_hash = hex::encode(blake3::hash(b"different content").as_bytes());

        let descriptor = ModelDescriptor {
            tier: ModelTier::Small,
            filename: filename.clone(),
            blake3_hex: expected_hash,
            download_url: Some("https://example.invalid/should-be-called".to_string()),
            size_bytes: 16,
        };
        let mgr =
            ModelManager::new(dir.path().to_path_buf(), vec![descriptor], 16384, 8192).unwrap();

        let err = mgr
            .download_model(ModelTier::Small, DownloadCallbacks::default())
            .expect_err("must NOT use fast path when partial hash is wrong");
        // It must have reached download_to_file and been rejected by the
        // host allow-list — proving the fast path was bypassed.
        assert!(
            err.to_string().to_lowercase().contains("host"),
            "expected host-not-allowed error from download_to_file; got: {err}"
        );
        // Final path must NOT exist (we did NOT rename a wrong-hash file).
        assert!(!dir.path().join(&filename).is_file());
    }
}
