//! Boot-time file integrity verification.
//!
//! The bridge's `validate_at_boot` calls into this module to check
//! that every model file on disk matches its compile-time-pinned
//! BLAKE3 hash. Files that pass verification are reported as
//! [`ModelVerificationStatus::Verified`]; failures fall into one of
//! four well-defined buckets ([`ModelVerificationStatus::Missing`],
//! [`ModelVerificationStatus::Mismatch`],
//! [`ModelVerificationStatus::ReadError`], or
//! [`ModelVerificationStatus::SizeMismatch`]) so the bridge can
//! decide whether to quarantine, warn, or hard-fail per failure mode.
//!
//! ## Why a separate type instead of `Result<(), Err>`?
//!
//! Boot verification is **not all-or-nothing**. A missing model
//! should not block the binary from booting (the user might not have
//! downloaded that tier yet); a hash mismatch on a 5 GB GGUF should
//! quarantine the file but allow the bridge to start up so the user
//! can re-download. Encoding the per-file outcome in an enum lets the
//! bridge make those policy decisions explicitly rather than
//! collapsing them into a generic `IntegrityError`.
//!
//! [`verify_files_against_pins`] therefore returns a `Vec` of
//! outcomes (never `Err` for per-file problems); it only fails as a
//! whole if its **input** was invalid (e.g. duplicate pin IDs).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::blake3_digest::{blake3_file, Blake3Digest, Blake3DigestError};

/// A compile-time pin describing a file that boot-time verification
/// must check. The bridge constructs one of these per descriptor it
/// knows about; `id` is the descriptor key (e.g. `"text-medium"` or
/// `"image-gen.sd15-fp16"`).
#[derive(Debug, Clone)]
pub struct FilePin {
    /// Caller-defined identifier for this pin. Used purely for
    /// logging / surfacing in the verification report.
    pub id: String,
    /// Absolute (or workspace-relative) path to the file on disk.
    pub path: PathBuf,
    /// Compile-time-pinned BLAKE3 digest the file must match.
    pub expected_blake3: Blake3Digest,
    /// Compile-time-pinned byte size the file must match. `0` is
    /// treated as "size pin is not enforced" (matches the convention
    /// in `ai_models.json` for descriptors awaiting first download).
    pub expected_size_bytes: u64,
}

/// Per-file outcome from [`verify_files_against_pins`]. One of these
/// is returned for every pin the bridge submitted, including pins
/// whose files are not yet on disk.
#[derive(Debug, Clone)]
pub struct ModelVerificationOutcome {
    /// Pin identifier the outcome refers to.
    pub id: String,
    /// Path that was verified.
    pub path: PathBuf,
    /// Status bucket the file fell into.
    pub status: ModelVerificationStatus,
}

/// Possible outcomes for a single file during boot verification.
#[derive(Debug, Clone)]
pub enum ModelVerificationStatus {
    /// File exists on disk, size matches, BLAKE3 matches the pin.
    Verified,
    /// File is not present on disk. Not necessarily an error — the
    /// user may simply have not downloaded that tier yet — but the
    /// bridge will refuse to spawn the sidecar against it.
    Missing,
    /// File exists but its size differs from the pin. Logged before
    /// the hash is even attempted (cheap fast-path).
    SizeMismatch {
        /// Size the pin claims the file should be.
        expected_bytes: u64,
        /// Actual on-disk size.
        actual_bytes: u64,
    },
    /// File exists, size matches, but BLAKE3 does not match the pin.
    /// This is the integrity-failure path — the bridge should
    /// quarantine the file.
    Mismatch {
        /// Pinned hash.
        expected: String,
        /// Recomputed hash of the file on disk.
        got: String,
    },
    /// File exists but we couldn't read it (permission denied,
    /// disk I/O error, etc.). Surfaced to the bridge so it can
    /// distinguish "tampered" from "broken disk".
    ReadError {
        /// Underlying error string (the original `io::Error` is not
        /// preserved because the report needs to be `Clone` for
        /// logging / forwarding).
        error: String,
    },
}

impl ModelVerificationStatus {
    /// True for the only status that means "this file is safe to use".
    #[must_use]
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified)
    }

    /// True for failure modes that should quarantine the file
    /// (tampering or corruption suspected). Missing files and read
    /// errors are *not* quarantine triggers — the former is normal,
    /// the latter is a transient I/O issue.
    #[must_use]
    pub fn is_quarantine_trigger(&self) -> bool {
        matches!(self, Self::SizeMismatch { .. } | Self::Mismatch { .. })
    }
}

/// Top-level failure cases for [`verify_files_against_pins`]. These
/// are misuses of the API by the **caller**, not per-file
/// verification outcomes.
#[derive(Debug, Error)]
pub enum VerifyFilesError {
    /// Two pins were submitted with the same `id`. The verifier
    /// refuses to run rather than silently dropping one of them.
    #[error("duplicate FilePin id: {0}")]
    DuplicateId(String),
}

/// A small `serde`-friendly report shape the bridge can persist /
/// log without dragging the `FilePin` type through its API surface.
/// Not used inside this crate; exported for the bridge's startup
/// telemetry path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    /// One entry per pin checked.
    pub entries: Vec<VerificationReportEntry>,
    /// Convenience flag set when **every** entry is verified.
    pub all_verified: bool,
}

/// `serde`-friendly mirror of [`ModelVerificationOutcome`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReportEntry {
    /// Pin identifier.
    pub id: String,
    /// Path that was verified, as a UTF-8 string.
    pub path: String,
    /// One of `verified | missing | size-mismatch | mismatch | read-error`.
    pub status: String,
    /// Human-readable detail describing the status; empty for verified.
    pub detail: String,
}

impl VerificationReport {
    /// Construct from a slice of outcomes.
    #[must_use]
    pub fn from_outcomes(outcomes: &[ModelVerificationOutcome]) -> Self {
        let all_verified = outcomes
            .iter()
            .all(|o| matches!(o.status, ModelVerificationStatus::Verified));
        let entries = outcomes
            .iter()
            .map(|o| {
                let (status, detail) = match &o.status {
                    ModelVerificationStatus::Verified => ("verified", String::new()),
                    ModelVerificationStatus::Missing => ("missing", String::new()),
                    ModelVerificationStatus::SizeMismatch {
                        expected_bytes,
                        actual_bytes,
                    } => (
                        "size-mismatch",
                        format!("expected {expected_bytes} bytes, got {actual_bytes}"),
                    ),
                    ModelVerificationStatus::Mismatch { expected, got } => {
                        ("mismatch", format!("expected blake3 {expected}, got {got}"))
                    }
                    ModelVerificationStatus::ReadError { error } => ("read-error", error.clone()),
                };
                VerificationReportEntry {
                    id: o.id.clone(),
                    path: o.path.to_string_lossy().into_owned(),
                    status: status.to_string(),
                    detail,
                }
            })
            .collect();
        Self {
            entries,
            all_verified,
        }
    }
}

/// Walk every pin and return a per-pin outcome.
///
/// Per-file failures are folded into the returned `Vec` rather than
/// short-circuiting — the bridge needs the full report to decide
/// quarantine vs. ignore on a per-descriptor basis. The function
/// only returns `Err` when the caller's input is malformed (e.g.
/// two pins with the same `id`).
///
/// This is the function the bridge's `validate_at_boot` will call
/// once Task 26 wires it up; it is fully real working code and is
/// covered by the integration tests in this module against ad-hoc
/// temporary files.
pub fn verify_files_against_pins(
    pins: &[FilePin],
) -> Result<Vec<ModelVerificationOutcome>, VerifyFilesError> {
    // Reject duplicate ids before doing any I/O so the misuse is
    // caught even on a fully-cached disk.
    {
        let mut seen = std::collections::HashSet::new();
        for pin in pins {
            if !seen.insert(pin.id.clone()) {
                return Err(VerifyFilesError::DuplicateId(pin.id.clone()));
            }
        }
    }

    let mut outcomes = Vec::with_capacity(pins.len());
    for pin in pins {
        let outcome = verify_single_pin(pin);
        if !outcome.status.is_verified() {
            // Anything non-verified is interesting enough to warn at
            // boot — the bridge can downgrade specific cases (e.g.
            // `Missing` for a tier the user simply hasn't downloaded).
            warn!(
                pin_id = pin.id.as_str(),
                path = ?pin.path,
                status = ?outcome.status,
                "boot-time integrity verification produced non-Verified outcome"
            );
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

fn verify_single_pin(pin: &FilePin) -> ModelVerificationOutcome {
    let status = match std::fs::metadata(&pin.path) {
        Ok(meta) => {
            if !meta.is_file() {
                ModelVerificationStatus::Missing
            } else if pin.expected_size_bytes != 0 && meta.len() != pin.expected_size_bytes {
                ModelVerificationStatus::SizeMismatch {
                    expected_bytes: pin.expected_size_bytes,
                    actual_bytes: meta.len(),
                }
            } else {
                match blake3_file(&pin.path) {
                    Ok(actual) => {
                        if actual == pin.expected_blake3 {
                            ModelVerificationStatus::Verified
                        } else {
                            ModelVerificationStatus::Mismatch {
                                expected: pin.expected_blake3.to_hex(),
                                got: actual.to_hex(),
                            }
                        }
                    }
                    Err(Blake3DigestError::Io { source }) => ModelVerificationStatus::ReadError {
                        error: source.to_string(),
                    },
                    Err(other) => ModelVerificationStatus::ReadError {
                        error: other.to_string(),
                    },
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ModelVerificationStatus::Missing,
        Err(e) => ModelVerificationStatus::ReadError {
            error: e.to_string(),
        },
    };
    ModelVerificationOutcome {
        id: pin.id.clone(),
        path: pin.path.clone(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const HELLO_WORLD_BLAKE3: &str =
        "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24";

    fn write_hello(path: &std::path::Path) -> u64 {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"hello world").unwrap();
        f.flush().unwrap();
        std::fs::metadata(path).unwrap().len()
    }

    #[test]
    fn verify_files_against_pins_reports_verified_for_matching_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        let size = write_hello(&path);

        let pin = FilePin {
            id: "text-small".into(),
            path: path.clone(),
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: size,
        };

        let outcomes = verify_files_against_pins(&[pin]).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].status.is_verified());
    }

    #[test]
    fn verify_files_against_pins_reports_missing_for_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.bin");

        let pin = FilePin {
            id: "text-medium".into(),
            path: missing,
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: 11,
        };

        let outcomes = verify_files_against_pins(&[pin]).unwrap();
        assert!(matches!(
            outcomes[0].status,
            ModelVerificationStatus::Missing
        ));
        assert!(!outcomes[0].status.is_quarantine_trigger());
    }

    #[test]
    fn verify_files_against_pins_reports_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        write_hello(&path); // actual size: 11

        let pin = FilePin {
            id: "text-large".into(),
            path,
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: 9_999,
        };

        let outcomes = verify_files_against_pins(&[pin]).unwrap();
        match &outcomes[0].status {
            ModelVerificationStatus::SizeMismatch {
                expected_bytes,
                actual_bytes,
            } => {
                assert_eq!(*expected_bytes, 9_999);
                assert_eq!(*actual_bytes, 11);
            }
            other => panic!("expected SizeMismatch, got {other:?}"),
        }
        assert!(outcomes[0].status.is_quarantine_trigger());
    }

    #[test]
    fn verify_files_against_pins_reports_blake3_mismatch_when_size_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tampered.bin");
        // Write 11 bytes ("DIFFERENT11") that differ from "hello world"
        // — both are 11 bytes so the size guard passes and we exercise
        // the hash-mismatch path.
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"DIFFERENT11").unwrap();
        f.flush().unwrap();

        let pin = FilePin {
            id: "tampered".into(),
            path,
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: 11,
        };

        let outcomes = verify_files_against_pins(&[pin]).unwrap();
        match &outcomes[0].status {
            ModelVerificationStatus::Mismatch { expected, got } => {
                assert_eq!(expected, HELLO_WORLD_BLAKE3);
                assert_ne!(got, HELLO_WORLD_BLAKE3);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
        assert!(outcomes[0].status.is_quarantine_trigger());
    }

    #[test]
    fn verify_files_against_pins_skips_size_check_when_pin_is_zero() {
        // A descriptor with `size_bytes: 0` means "size pin not yet
        // enforced" (this matches the convention in ai_models.json
        // for descriptors awaiting first download). The hash check
        // should still run.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        write_hello(&path);

        let pin = FilePin {
            id: "unsized".into(),
            path,
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: 0,
        };

        let outcomes = verify_files_against_pins(&[pin]).unwrap();
        assert!(outcomes[0].status.is_verified());
    }

    #[test]
    fn verify_files_against_pins_rejects_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        write_hello(&path);

        let pin1 = FilePin {
            id: "dup".into(),
            path: path.clone(),
            expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
            expected_size_bytes: 11,
        };
        let pin2 = pin1.clone();

        let err = verify_files_against_pins(&[pin1, pin2]).unwrap_err();
        match err {
            VerifyFilesError::DuplicateId(id) => assert_eq!(id, "dup"),
        }
    }

    #[test]
    fn verify_files_against_pins_handles_multiple_pins_independently() {
        let dir = tempfile::tempdir().unwrap();
        let ok_path = dir.path().join("ok.bin");
        write_hello(&ok_path);
        let missing_path = dir.path().join("gone.bin");

        let pins = vec![
            FilePin {
                id: "ok".into(),
                path: ok_path,
                expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
                expected_size_bytes: 11,
            },
            FilePin {
                id: "gone".into(),
                path: missing_path,
                expected_blake3: Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap(),
                expected_size_bytes: 11,
            },
        ];

        let outcomes = verify_files_against_pins(&pins).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].status.is_verified());
        assert!(matches!(
            outcomes[1].status,
            ModelVerificationStatus::Missing
        ));
    }

    #[test]
    fn verification_report_summarizes_all_verified() {
        let outcomes = vec![ModelVerificationOutcome {
            id: "ok".into(),
            path: PathBuf::from("/tmp/ok.bin"),
            status: ModelVerificationStatus::Verified,
        }];
        let report = VerificationReport::from_outcomes(&outcomes);
        assert!(report.all_verified);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, "verified");
        assert!(report.entries[0].detail.is_empty());
    }

    #[test]
    fn verification_report_summarizes_mismatch_with_detail() {
        let outcomes = vec![ModelVerificationOutcome {
            id: "bad".into(),
            path: PathBuf::from("/tmp/bad.bin"),
            status: ModelVerificationStatus::Mismatch {
                expected: "deadbeef".repeat(8),
                got: "cafebabe".repeat(8),
            },
        }];
        let report = VerificationReport::from_outcomes(&outcomes);
        assert!(!report.all_verified);
        assert_eq!(report.entries[0].status, "mismatch");
        assert!(report.entries[0]
            .detail
            .contains("expected blake3 deadbeef"));
        assert!(report.entries[0].detail.contains("got cafebabe"));
    }

    #[test]
    fn verification_report_summarizes_size_mismatch_with_detail() {
        let outcomes = vec![ModelVerificationOutcome {
            id: "size".into(),
            path: PathBuf::from("/tmp/size.bin"),
            status: ModelVerificationStatus::SizeMismatch {
                expected_bytes: 1000,
                actual_bytes: 500,
            },
        }];
        let report = VerificationReport::from_outcomes(&outcomes);
        assert!(!report.all_verified);
        assert_eq!(report.entries[0].status, "size-mismatch");
        assert!(report.entries[0].detail.contains("expected 1000 bytes"));
        assert!(report.entries[0].detail.contains("got 500"));
    }
}
