//! Model tier selection and lifecycle management (Phase 12 Task 19).
//!
//! Selects the appropriate model size (1.7B / 4B / 8B) based on the host's
//! hardware profile (total RAM + GPU VRAM), downloads the model file on
//! first use from a configured URL, and verifies its BLAKE3 checksum.
//!
//! The manager is stateless across sessions — it reads the models directory
//! on startup, checks what's available, and only downloads what's missing.
//! Tier switching at runtime is supported by updating the `RuntimeConfig`
//! returned by [`ModelManager::active_config`] and restarting the sidecar.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}

/// Model size tiers matching the sidecar's supported quantisations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// 1.7B parameter model — runs on ≥ 4 GB RAM, no GPU required.
    Small,
    /// 4B parameter model — needs ≥ 8 GB RAM or ≥ 4 GB VRAM.
    Medium,
    /// 8B parameter model — needs ≥ 16 GB RAM or ≥ 8 GB VRAM.
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

    /// The expected GGUF filename for this tier.
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Small => "prismml-1.7b-q4_k_m.gguf",
            Self::Medium => "prismml-4b-q4_k_m.gguf",
            Self::Large => "prismml-8b-q4_k_m.gguf",
        }
    }

    /// Context window size appropriate for this tier.
    pub fn context_tokens(&self) -> u32 {
        match self {
            Self::Small => 2048,
            Self::Medium => 4096,
            Self::Large => 4096,
        }
    }
}

/// Descriptor for a model file, including its expected BLAKE3 checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    pub fn active_tier(&self) -> ModelTier {
        self.active_tier
    }

    /// Switch to a different tier at runtime (e.g. from Settings).
    pub fn set_tier(&mut self, tier: ModelTier) {
        self.active_tier = tier;
    }

    /// Path to the active model file.
    pub fn active_model_path(&self) -> PathBuf {
        self.models_dir.join(self.active_tier.filename())
    }

    /// Check whether the active model file exists on disk.
    pub fn is_active_model_available(&self) -> bool {
        self.active_model_path().is_file()
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
        let path = self.active_model_path();
        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.tier == self.active_tier);
        let Some(desc) = descriptor else {
            return Ok(true); // No descriptor → skip verification.
        };
        if !path.is_file() {
            return Err(ModelManagerError::ModelNotFound(
                path.display().to_string(),
            ));
        }
        let actual = blake3_file(&path)?;
        if actual != desc.blake3_hex {
            return Err(ModelManagerError::ChecksumMismatch {
                path: path.display().to_string(),
                expected: desc.blake3_hex.clone(),
                actual,
            });
        }
        Ok(true)
    }

    /// Write model bytes to the models directory for the given tier,
    /// verifying the BLAKE3 checksum. Used by the download path.
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
        let path = self.models_dir.join(tier.filename());
        let mut file = std::fs::File::create(&path)?;
        file.write_all(data)?;
        file.flush()?;
        Ok(path)
    }

    /// List all available (on-disk) models.
    pub fn available_models(&self) -> Vec<(ModelTier, PathBuf)> {
        let tiers = [ModelTier::Small, ModelTier::Medium, ModelTier::Large];
        tiers
            .into_iter()
            .filter_map(|t| {
                let path = self.models_dir.join(t.filename());
                path.is_file().then_some((t, path))
            })
            .collect()
    }
}

/// Select the best model tier based on hardware. Prefers the largest tier
/// the hardware can comfortably support.
///
/// Heuristic:
///   - VRAM ≥ 8 GB or RAM ≥ 16 GB → Large (8B)
///   - VRAM ≥ 4 GB or RAM ≥  8 GB → Medium (4B)
///   - RAM ≥ 4 GB                  → Small (1.7B)
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
    fn model_tier_filenames_are_consistent() {
        assert!(ModelTier::Small.filename().contains("1.7b"));
        assert!(ModelTier::Medium.filename().contains("4b"));
        assert!(ModelTier::Large.filename().contains("8b"));
    }

    #[test]
    fn install_model_verifies_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        let data = b"fake model data for testing";
        let hash = hex::encode(blake3::hash(data).as_bytes());
        let path = mgr
            .install_model(ModelTier::Small, data, &hash)
            .unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }

    #[test]
    fn install_model_rejects_bad_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        let result = mgr.install_model(ModelTier::Small, b"data", "bad_hash");
        assert!(matches!(result, Err(ModelManagerError::ChecksumMismatch { .. })));
    }

    #[test]
    fn active_config_uses_selected_tier() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ModelManager::new(dir.path().to_path_buf(), vec![], 16384, 8192).unwrap();
        assert_eq!(mgr.active_tier(), ModelTier::Large);
        mgr.set_tier(ModelTier::Small);
        assert_eq!(mgr.active_tier(), ModelTier::Small);
        let cfg = mgr.active_config();
        assert!(cfg.model_path.to_string_lossy().contains("1.7b"));
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
}
