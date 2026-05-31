//! Image-gen model manager.
//!
//! Mirrors [`crate::model_manager`] for the image-gen side, with one
//! key simplification: there is **one** image-gen model today
//! (a small SD/Flux GGUF), not a tier ladder. The bridge surface is
//! still structured around a [`ImageGenModelDescriptor`] so a future
//! Phase 18 Group D step can promote the descriptor to
//! `ai_models.json` and serve multiple image-gen variants
//! (SD-turbo / Flux-schnell / native bonsai-image) without breaking
//! the runtime contract here.
//!
//! Responsibilities:
//!
//!   * Resolve the on-disk path the active image-gen model is
//!     expected to live at (`<models_dir>/<descriptor.filename>`).
//!   * Verify the on-disk file's BLAKE3 matches the descriptor's
//!     pinned digest. Pinning at compile time is the same security
//!     posture as the text models: an attacker who can swap the
//!     remote URL or its body still has to break BLAKE3 collision
//!     resistance to substitute weights.
//!   * Download the model with HTTPS resume + atomic rename. The
//!     download path delegates to the shared
//!     [`crate::model_download::download_to_file`] entry point so the
//!     host allow-list (HuggingFace + leading-dot rejection) and
//!     redirect validation apply uniformly to text and image
//!     downloads.
//!
//! The download URL is **not** populated in the shipped default
//! descriptor because the Phase 18 Group C decision was to design
//! the bridge surface against `stable-diffusion.cpp` first and ship
//! the canonical small-SD-GGUF URL + BLAKE3 in the same commit that
//! ships [`crate::image_gen::sidecar::DEFAULT_IMAGE_GEN_BIN`]'s
//! vendored binary. Until then the manager surfaces
//! [`ImageGenModelManagerError::NoDownloadUrl`] when a caller tries
//! to download without first registering a URL via
//! [`ImageGenModelManager::set_download_url`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model_download::{self, DownloadError, ProgressCallback};
use crate::model_manager::{DownloadCallbacks, DownloadState};

#[derive(Debug, Error)]
pub enum ImageGenModelManagerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("download: {0}")]
    Download(String),
    #[error("model file not found: {0}")]
    ModelNotFound(String),
    #[error("BLAKE3 mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("image-gen descriptor has no download_url; call set_download_url first")]
    NoDownloadUrl,
}

impl From<DownloadError> for ImageGenModelManagerError {
    fn from(value: DownloadError) -> Self {
        Self::Download(value.to_string())
    }
}

/// Pinned descriptor for one image-gen model.
///
/// `download_url` is `Option<String>` because Phase 18 Group C
/// deliberately ships without a remote URL — the user must side-load
/// the model (or wait for Group D to ship the canonical
/// HuggingFace URL alongside the bundled `sd-server` binary). When
/// `download_url` is `None`, [`ImageGenModelManager::download_model`]
/// returns [`ImageGenModelManagerError::NoDownloadUrl`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageGenModelDescriptor {
    /// Filename on disk under `models_dir`. The runtime's spawn args
    /// pass `models_dir.join(filename)` as `--model`.
    pub filename: String,
    /// Pinned BLAKE3 hex digest. Empty string means "no checksum
    /// available yet" — the verify path returns `Ok(false)` in that
    /// case (consistent with the text manager's
    /// [`crate::model_manager::ModelManager::verify_file`] contract).
    pub blake3_hex: String,
    /// Expected total bytes on disk. Used for the
    /// `path.is_file() && size == expected_size` availability
    /// shortcut, same shape the text manager exposes via
    /// `AiModelTierInfo::available`.
    pub size_bytes: u64,
    /// HuggingFace LFS pointer URL. Allowed-host validated via
    /// [`crate::model_download::download_to_file`] before any socket
    /// is opened. `None` until Group D wires the canonical URL.
    pub download_url: Option<String>,
    /// Optional separate VAE descriptor. Many SD-family GGUFs embed
    /// the VAE; Flux-schnell distillations sometimes ship it as a
    /// sidecar `.safetensors`. When set, the sidecar's spawn args
    /// include `--vae <vae_path>`.
    pub vae_filename: Option<String>,
}

/// Default image-gen model descriptor.
///
/// Phase 18 Group C ships an empty `download_url`: the bridge
/// surface, runtime, and renderer are all live and exercised by
/// tests, but the actual remote model URL is deferred to Group D
/// alongside the canonical small-SD-GGUF + bundled binary. Until
/// then the user side-loads a `.gguf` into `<models_dir>/` and the
/// runtime picks it up by filename.
///
/// `filename` and `blake3_hex` are placeholders that Group D will
/// replace with the canonical small-SD-GGUF entries. They are
/// deliberately non-empty so callers can exercise the
/// "file present + size matches" availability check end-to-end
/// against a side-loaded model.
pub const DEFAULT_IMAGE_GEN_MODEL: ImageGenModelDescriptor = ImageGenModelDescriptor {
    filename: String::new(),
    blake3_hex: String::new(),
    size_bytes: 0,
    download_url: None,
    vae_filename: None,
};

/// Manages the image-gen model file on disk.
#[derive(Debug, Clone)]
pub struct ImageGenModelManager {
    models_dir: PathBuf,
    descriptor: ImageGenModelDescriptor,
}

impl ImageGenModelManager {
    /// Build a manager rooted at `models_dir` with `descriptor` as
    /// the registered model. `models_dir` is created on first
    /// download.
    pub fn new(models_dir: PathBuf, descriptor: ImageGenModelDescriptor) -> Self {
        Self {
            models_dir,
            descriptor,
        }
    }

    /// Build a manager seeded with [`DEFAULT_IMAGE_GEN_MODEL`].
    pub fn with_default_descriptor(models_dir: PathBuf) -> Self {
        Self::new(models_dir, default_image_gen_descriptor())
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn descriptor(&self) -> &ImageGenModelDescriptor {
        &self.descriptor
    }

    /// Replace the active descriptor (e.g. when the renderer's
    /// model-picker selects a different image-gen variant).
    pub fn set_descriptor(&mut self, descriptor: ImageGenModelDescriptor) {
        self.descriptor = descriptor;
    }

    /// Register a remote download URL for the active descriptor.
    /// Used by the Group D wizard / by `ai_models.json` loaders so
    /// the same descriptor that ships with no URL today can be
    /// promoted to "downloadable" without a code change.
    pub fn set_download_url(&mut self, url: impl Into<String>) {
        self.descriptor.download_url = Some(url.into());
    }

    /// Absolute path the configured model file is expected to live
    /// at.
    pub fn model_path(&self) -> PathBuf {
        self.models_dir.join(&self.descriptor.filename)
    }

    /// Absolute path the optional VAE file is expected to live at,
    /// or `None` when the descriptor does not declare one.
    pub fn vae_path(&self) -> Option<PathBuf> {
        self.descriptor
            .vae_filename
            .as_ref()
            .map(|f| self.models_dir.join(f))
    }

    /// `true` only when both
    ///   * the descriptor declares a non-empty filename, **and**
    ///   * a regular file of the descriptor's expected size sits at
    ///     `model_path()`.
    ///
    /// This is the same availability contract the text-side bridge
    /// publishes via `AiModelTierInfo::available`. BLAKE3 is
    /// **not** checked here — verification is reserved for the
    /// download / verify path so the renderer's per-frame poll
    /// stays cheap.
    pub fn is_available(&self) -> bool {
        if self.descriptor.filename.is_empty() || self.descriptor.size_bytes == 0 {
            return false;
        }
        let path = self.model_path();
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        meta.is_file() && meta.len() == self.descriptor.size_bytes
    }

    /// Byte length of the on-disk file, or 0 when missing. Used by
    /// the renderer to render a "X.X MiB downloaded of Y.Y MiB"
    /// label without re-hashing.
    pub fn size_on_disk(&self) -> u64 {
        std::fs::metadata(self.model_path()).map_or(0, |m| m.len())
    }

    /// Verify the on-disk file's BLAKE3 matches the descriptor's
    /// pinned digest. Returns `Ok(true)` only on a hit;
    /// `Ok(false)` when the descriptor has no pinned digest (so
    /// verification is a no-op);
    /// [`ImageGenModelManagerError::ChecksumMismatch`] /
    /// [`ImageGenModelManagerError::ModelNotFound`] otherwise.
    pub fn verify_checksum(&self) -> Result<bool, ImageGenModelManagerError> {
        let path = self.model_path();
        if !path.is_file() {
            return Err(ImageGenModelManagerError::ModelNotFound(
                path.display().to_string(),
            ));
        }
        if self.descriptor.blake3_hex.is_empty() {
            return Ok(false);
        }
        let actual = blake3_file(&path)?;
        if actual != self.descriptor.blake3_hex {
            return Err(ImageGenModelManagerError::ChecksumMismatch {
                path: path.display().to_string(),
                expected: self.descriptor.blake3_hex.clone(),
                actual,
            });
        }
        Ok(true)
    }

    /// Download the active descriptor's model file from its
    /// `download_url` to `models_dir`, verify BLAKE3, atomically
    /// rename the partial into place. Same lifecycle / callback
    /// shape as [`crate::model_manager::ModelManager::download_model`]
    /// — we reuse [`DownloadCallbacks`] so the bridge can fan-out
    /// the same progress slot it uses for text downloads without
    /// allocating a parallel snapshot type.
    pub fn download_model(
        &self,
        callbacks: DownloadCallbacks,
    ) -> Result<PathBuf, ImageGenModelManagerError> {
        let Some(url) = self.descriptor.download_url.clone() else {
            return Err(ImageGenModelManagerError::NoDownloadUrl);
        };
        std::fs::create_dir_all(&self.models_dir)?;
        let final_path = self.model_path();
        // Fast path: already on disk and verified.
        if final_path.is_file() && !self.descriptor.blake3_hex.is_empty() {
            let actual = blake3_file(&final_path)?;
            if actual == self.descriptor.blake3_hex {
                fire_state(&callbacks, DownloadState::Completed);
                return Ok(final_path);
            }
            // Bad on-disk checksum — wipe and re-download.
            std::fs::remove_file(&final_path)?;
        }
        let partial_path = self
            .models_dir
            .join(format!("{}.partial", self.descriptor.filename));
        // Recovery fast path: a previous attempt may have finished
        // the transfer (correct BLAKE3) but crashed between verify
        // and atomic rename. Without this check, the next call would
        // resume from the end of the file, send
        // `Range: bytes=<total>-`, get HTTP 416, and treat that as a
        // fatal error — leaving the partial in place so every
        // subsequent retry hits the same 416 (permanent failure).
        // Hash the partial first; if it already matches, skip the
        // HTTP transfer entirely and proceed straight to rename. The
        // BLAKE3 hash is the authoritative completeness check, not
        // the file size. Parallels the text-side fix in
        // [`crate::model_manager::ModelManager::download_model`].
        if partial_path.is_file() && !self.descriptor.blake3_hex.is_empty() {
            if let Ok(actual) = blake3_file(&partial_path) {
                if actual == self.descriptor.blake3_hex {
                    fire_state(&callbacks, DownloadState::Verifying);
                    if let Err(e) = std::fs::rename(&partial_path, &final_path) {
                        let msg = e.to_string();
                        fire_state(
                            &callbacks,
                            DownloadState::Failed {
                                stage: "rename",
                                msg: msg.clone(),
                            },
                        );
                        return Err(e.into());
                    }
                    fire_state(&callbacks, DownloadState::Completed);
                    return Ok(final_path);
                }
            }
        }
        fire_state(&callbacks, DownloadState::Downloading);
        if let Err(e) = model_download::download_to_file(
            &url,
            &partial_path,
            self.descriptor.size_bytes,
            callbacks.on_progress.clone(),
        ) {
            let msg = e.to_string();
            fire_state(
                &callbacks,
                DownloadState::Failed {
                    stage: "download",
                    msg: msg.clone(),
                },
            );
            return Err(ImageGenModelManagerError::Download(msg));
        }
        fire_state(&callbacks, DownloadState::Verifying);
        let actual = match blake3_file(&partial_path) {
            Ok(h) => h,
            Err(e) => {
                let msg = e.to_string();
                fire_state(
                    &callbacks,
                    DownloadState::Failed {
                        stage: "verify",
                        msg: msg.clone(),
                    },
                );
                return Err(e);
            }
        };
        if !self.descriptor.blake3_hex.is_empty() && actual != self.descriptor.blake3_hex {
            let _ = std::fs::remove_file(&partial_path);
            let err = ImageGenModelManagerError::ChecksumMismatch {
                path: partial_path.display().to_string(),
                expected: self.descriptor.blake3_hex.clone(),
                actual,
            };
            fire_state(
                &callbacks,
                DownloadState::Failed {
                    stage: "verify",
                    msg: err.to_string(),
                },
            );
            return Err(err);
        }
        if let Err(e) = std::fs::rename(&partial_path, &final_path) {
            let msg = e.to_string();
            fire_state(
                &callbacks,
                DownloadState::Failed {
                    stage: "rename",
                    msg: msg.clone(),
                },
            );
            return Err(e.into());
        }
        fire_state(&callbacks, DownloadState::Completed);
        Ok(final_path)
    }
}

/// Cloneable default — we can't `const` the descriptor because
/// `Option<String>` etc. are non-const, so this is the runtime
/// alternative. Equivalent to [`DEFAULT_IMAGE_GEN_MODEL`].
pub fn default_image_gen_descriptor() -> ImageGenModelDescriptor {
    ImageGenModelDescriptor {
        filename: String::new(),
        blake3_hex: String::new(),
        size_bytes: 0,
        download_url: None,
        vae_filename: None,
    }
}

fn fire_state(callbacks: &DownloadCallbacks, state: DownloadState) {
    if let Some(f) = &callbacks.on_state {
        f(state);
    }
}

fn blake3_file(path: &Path) -> Result<String, ImageGenModelManagerError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize().as_bytes()))
}

/// Convenience: build a [`ProgressCallback`] from a closure. Mirrors
/// [`crate::model_manager::progress_arc`] for image-gen call sites.
pub fn progress_arc<F>(f: F) -> ProgressCallback
where
    F: Fn(u64, u64) + Send + Sync + 'static,
{
    std::sync::Arc::new(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn default_descriptor_is_empty_so_download_must_be_explicitly_configured() {
        let mgr = ImageGenModelManager::with_default_descriptor(PathBuf::from("/tmp/aec-test"));
        assert!(mgr.descriptor().filename.is_empty());
        assert!(mgr.descriptor().download_url.is_none());
        assert!(!mgr.is_available());
        let err = mgr
            .download_model(DownloadCallbacks::default())
            .unwrap_err();
        assert!(matches!(err, ImageGenModelManagerError::NoDownloadUrl));
    }

    #[test]
    fn is_available_returns_true_when_file_present_with_expected_size() {
        let tmp = tmpdir();
        let descriptor = ImageGenModelDescriptor {
            filename: "test-sd.gguf".into(),
            blake3_hex: String::new(),
            size_bytes: 11,
            download_url: None,
            vae_filename: None,
        };
        let mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        // No file yet.
        assert!(!mgr.is_available());
        assert_eq!(mgr.size_on_disk(), 0);
        // Drop a file of the wrong size — still unavailable.
        let path = mgr.model_path();
        std::fs::write(&path, b"shortbytes").unwrap();
        assert!(!mgr.is_available());
        // Drop a file of the right size — now available.
        std::fs::write(&path, b"01234567890").unwrap(); // 11 bytes
        assert!(mgr.is_available());
        assert_eq!(mgr.size_on_disk(), 11);
    }

    #[test]
    fn vae_path_resolves_against_models_dir() {
        let tmp = tmpdir();
        let descriptor = ImageGenModelDescriptor {
            filename: "model.gguf".into(),
            blake3_hex: String::new(),
            size_bytes: 0,
            download_url: None,
            vae_filename: Some("vae.safetensors".into()),
        };
        let mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        assert_eq!(mgr.vae_path().unwrap(), tmp.path().join("vae.safetensors"));
    }

    #[test]
    fn vae_path_is_none_when_descriptor_omits_it() {
        let tmp = tmpdir();
        let descriptor = ImageGenModelDescriptor {
            filename: "model.gguf".into(),
            blake3_hex: String::new(),
            size_bytes: 0,
            download_url: None,
            vae_filename: None,
        };
        let mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        assert!(mgr.vae_path().is_none());
    }

    #[test]
    fn verify_checksum_passes_when_blake3_matches() {
        let tmp = tmpdir();
        let payload = b"hello image-gen";
        let expected = hex::encode(blake3::hash(payload).as_bytes());
        let descriptor = ImageGenModelDescriptor {
            filename: "m.gguf".into(),
            blake3_hex: expected.clone(),
            size_bytes: payload.len() as u64,
            download_url: None,
            vae_filename: None,
        };
        let mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        let path = mgr.model_path();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(payload).unwrap();
        drop(f);
        assert!(mgr.verify_checksum().unwrap());
    }

    #[test]
    fn verify_checksum_rejects_corrupt_file() {
        let tmp = tmpdir();
        let descriptor = ImageGenModelDescriptor {
            filename: "m.gguf".into(),
            // Pin to a non-matching digest.
            blake3_hex: "deadbeef".repeat(8),
            size_bytes: 4,
            download_url: None,
            vae_filename: None,
        };
        let mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        std::fs::write(mgr.model_path(), b"abcd").unwrap();
        let err = mgr.verify_checksum().unwrap_err();
        match err {
            ImageGenModelManagerError::ChecksumMismatch {
                expected, actual, ..
            } => {
                assert!(expected.starts_with("deadbeef"));
                assert_ne!(expected, actual);
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }

    #[test]
    fn set_descriptor_replaces_filename_and_url() {
        let tmp = tmpdir();
        let mut mgr = ImageGenModelManager::with_default_descriptor(tmp.path().to_path_buf());
        let new_descriptor = ImageGenModelDescriptor {
            filename: "swapped.gguf".into(),
            blake3_hex: "deadbeef".into(),
            size_bytes: 42,
            download_url: Some("https://huggingface.co/x/y/resolve/main/swapped.gguf".into()),
            vae_filename: None,
        };
        mgr.set_descriptor(new_descriptor);
        assert_eq!(mgr.descriptor().filename, "swapped.gguf");
        assert_eq!(
            mgr.descriptor().download_url.as_deref(),
            Some("https://huggingface.co/x/y/resolve/main/swapped.gguf")
        );
    }

    #[test]
    fn set_download_url_promotes_a_pre_existing_descriptor() {
        let tmp = tmpdir();
        let descriptor = ImageGenModelDescriptor {
            filename: "shipped.gguf".into(),
            blake3_hex: String::new(),
            size_bytes: 0,
            download_url: None,
            vae_filename: None,
        };
        let mut mgr = ImageGenModelManager::new(tmp.path().to_path_buf(), descriptor);
        assert!(mgr.descriptor().download_url.is_none());
        mgr.set_download_url("https://huggingface.co/x/y/resolve/main/shipped.gguf");
        assert_eq!(
            mgr.descriptor().download_url.as_deref(),
            Some("https://huggingface.co/x/y/resolve/main/shipped.gguf"),
        );
    }
}
