//! Typed BLAKE3 digest + streaming file hasher.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Length of a BLAKE3 hash in hex characters (32-byte digest → 64 hex).
pub const BLAKE3_HEX_LEN: usize = 64;

/// Streaming chunk size for [`blake3_file`]. 64 KiB matches
/// `std::fs::File`'s default buffer size and the chunk size used by
/// `aec_ai::model_manager::blake3_file` (which this crate is intended
/// to eventually replace once the bridge crate depends on
/// `aec_integrity` directly). Keeping the two chunk sizes equal means
/// download → verify behaves identically (page cache hit ratio,
/// I/O syscall count) regardless of which entrypoint computed the
/// hash.
pub const BLAKE3_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Strongly-typed wrapper around a BLAKE3 digest.
///
/// The on-disk and over-the-wire representation is **lowercase hex
/// without `0x` prefix and without separators** — the same shape the
/// `blake3` CLI emits and what `crates/aec_ai/data/ai_models.json`
/// stores. Equality is on the raw 32-byte digest, so a hex literal
/// with mixed case still compares equal after `from_hex`.
///
/// `serde` round-trips via the hex form; the raw `[u8; 32]` is **not**
/// exposed at the wire boundary because we want JSON manifests (model
/// registries, future signed update manifests) to remain
/// human-inspectable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blake3Digest(blake3::Hash);

/// Errors that can occur while parsing or computing a [`Blake3Digest`].
#[derive(Debug, Error)]
pub enum Blake3DigestError {
    /// The provided hex string was not the expected 64 characters.
    #[error("BLAKE3 hex must be exactly {BLAKE3_HEX_LEN} characters (got {got})")]
    BadLength {
        /// Length of the malformed input.
        got: usize,
    },
    /// The hex string contained a non-hex character.
    #[error("BLAKE3 hex contains non-hex character at byte {byte_index}")]
    BadHexChar {
        /// 0-indexed byte position of the offending character.
        byte_index: usize,
    },
    /// I/O error reading the file under hash.
    #[error("BLAKE3 file hash I/O error: {source}")]
    Io {
        /// Underlying I/O error.
        #[from]
        source: io::Error,
    },
    /// Computed digest did not match the expected value.
    #[error("BLAKE3 digest mismatch: expected {expected}, got {got}")]
    Mismatch {
        /// Caller-provided pin.
        expected: String,
        /// Recomputed digest of the file on disk.
        got: String,
    },
}

impl Blake3Digest {
    /// Parse a lowercase or uppercase hex string into a digest. Returns
    /// [`Blake3DigestError::BadLength`] / [`Blake3DigestError::BadHexChar`]
    /// for malformed input rather than `Result<_, hex::FromHexError>`
    /// so the bridge layer can match on a single error enum.
    pub fn from_hex(hex_str: &str) -> Result<Self, Blake3DigestError> {
        if hex_str.len() != BLAKE3_HEX_LEN {
            return Err(Blake3DigestError::BadLength { got: hex_str.len() });
        }
        let mut out = [0u8; 32];
        // hex::decode_to_slice both validates and writes in one pass,
        // so we don't allocate a temporary Vec. The error mapping
        // surfaces the byte index for nicer error messages.
        hex::decode_to_slice(hex_str, &mut out).map_err(|e| match e {
            hex::FromHexError::OddLength => Blake3DigestError::BadLength { got: hex_str.len() },
            hex::FromHexError::InvalidStringLength => {
                Blake3DigestError::BadLength { got: hex_str.len() }
            }
            hex::FromHexError::InvalidHexCharacter { index, .. } => {
                Blake3DigestError::BadHexChar { byte_index: index }
            }
        })?;
        Ok(Self(blake3::Hash::from_bytes(out)))
    }

    /// Wrap an existing 32-byte digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(blake3::Hash::from_bytes(bytes))
    }

    /// Return the raw 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex representation, suitable for storage in JSON
    /// manifests and for logging.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.as_bytes())
    }

    /// Verify that this digest matches the given file's BLAKE3 hash.
    /// Streams the file in [`BLAKE3_STREAM_CHUNK_BYTES`] chunks; never
    /// reads the whole file into memory.
    ///
    /// On mismatch, returns [`Blake3DigestError::Mismatch`] with both
    /// values in hex so the caller can log the discrepancy without
    /// recomputing.
    pub fn verify_file(&self, path: &Path) -> Result<(), Blake3DigestError> {
        let actual = blake3_file(path)?;
        if actual == *self {
            Ok(())
        } else {
            Err(Blake3DigestError::Mismatch {
                expected: self.to_hex(),
                got: actual.to_hex(),
            })
        }
    }
}

impl fmt::Debug for Blake3Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Blake3Digest").field(&self.to_hex()).finish()
    }
}

impl fmt::Display for Blake3Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Blake3Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Blake3Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Compute the BLAKE3 digest of a file by streaming it in
/// [`BLAKE3_STREAM_CHUNK_BYTES`] chunks. Returns the digest as a
/// [`Blake3Digest`] (already validated to be 32 bytes; cannot be
/// constructed in any other way).
///
/// This is the canonical hasher for the bridge's boot-time
/// verification path (Task 26 will wire it in). Held out of
/// [`Blake3Digest::verify_file`] so callers that need the digest
/// itself — e.g. to log the actual hash on first download — don't
/// have to verify-then-recompute.
pub fn blake3_file(path: &Path) -> Result<Blake3Digest, Blake3DigestError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; BLAKE3_STREAM_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Blake3Digest(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `b"hello world"` → BLAKE3 hash (computed once via the `blake3`
    /// CLI; this is the regression pin for the streaming hasher).
    const HELLO_WORLD_BLAKE3: &str =
        "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24";

    #[test]
    fn blake3_digest_from_hex_round_trips_lowercase() {
        let digest = Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap();
        assert_eq!(digest.to_hex(), HELLO_WORLD_BLAKE3);
    }

    #[test]
    fn blake3_digest_from_hex_accepts_uppercase() {
        // Same bytes, uppercase. We normalize to lowercase on
        // `to_hex` regardless.
        let upper = HELLO_WORLD_BLAKE3.to_ascii_uppercase();
        let digest = Blake3Digest::from_hex(&upper).unwrap();
        assert_eq!(digest.to_hex(), HELLO_WORLD_BLAKE3);
    }

    #[test]
    fn blake3_digest_from_hex_rejects_short_input() {
        let err = Blake3Digest::from_hex("d74981ef").unwrap_err();
        match err {
            Blake3DigestError::BadLength { got } => assert_eq!(got, 8),
            other => panic!("expected BadLength, got {other:?}"),
        }
    }

    #[test]
    fn blake3_digest_from_hex_rejects_long_input() {
        let too_long = format!("{HELLO_WORLD_BLAKE3}00");
        let err = Blake3Digest::from_hex(&too_long).unwrap_err();
        match err {
            Blake3DigestError::BadLength { got } => assert_eq!(got, 66),
            other => panic!("expected BadLength, got {other:?}"),
        }
    }

    #[test]
    fn blake3_digest_from_hex_rejects_non_hex_chars() {
        // 64 chars but contains a 'g' (not a hex digit)
        let bad = "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e2g";
        let err = Blake3Digest::from_hex(bad).unwrap_err();
        match err {
            Blake3DigestError::BadHexChar { byte_index } => assert_eq!(byte_index, 63),
            other => panic!("expected BadHexChar, got {other:?}"),
        }
    }

    #[test]
    fn blake3_file_streams_hello_world() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world")
            .unwrap();

        let digest = blake3_file(&path).unwrap();
        assert_eq!(digest.to_hex(), HELLO_WORLD_BLAKE3);
    }

    #[test]
    fn blake3_file_streams_multi_chunk_input() {
        // Force more than one chunk through the streaming loop.
        // We don't pin the exact hash here — the property we care
        // about is that the streaming hasher produces the same result
        // as a single-shot hash over the same bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let mut file = std::fs::File::create(&path).unwrap();
        let pattern: Vec<u8> = (0u8..=255)
            .cycle()
            .take(BLAKE3_STREAM_CHUNK_BYTES * 3 + 17)
            .collect();
        file.write_all(&pattern).unwrap();
        drop(file);

        let streamed = blake3_file(&path).unwrap();
        let one_shot = blake3::hash(&pattern);
        assert_eq!(streamed.as_bytes(), one_shot.as_bytes());
    }

    #[test]
    fn blake3_file_io_error_when_path_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.bin");
        let err = blake3_file(&missing).unwrap_err();
        assert!(matches!(err, Blake3DigestError::Io { .. }));
    }

    #[test]
    fn verify_file_succeeds_on_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world")
            .unwrap();
        let expected = Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap();
        expected.verify_file(&path).expect("hash must match");
    }

    #[test]
    fn verify_file_returns_mismatch_with_both_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world TAMPERED")
            .unwrap();
        let expected = Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap();
        let err = expected.verify_file(&path).unwrap_err();
        match err {
            Blake3DigestError::Mismatch { expected: e, got } => {
                assert_eq!(e, HELLO_WORLD_BLAKE3);
                assert_ne!(got, HELLO_WORLD_BLAKE3);
                assert_eq!(got.len(), BLAKE3_HEX_LEN);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn serde_round_trip_via_hex_string() {
        let digest = Blake3Digest::from_hex(HELLO_WORLD_BLAKE3).unwrap();
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"{HELLO_WORLD_BLAKE3}\""));
        let parsed: Blake3Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, digest);
    }

    #[test]
    fn serde_rejects_malformed_hex_at_deserialize() {
        let bad_json = "\"not-a-real-hash\"";
        let err = serde_json::from_str::<Blake3Digest>(bad_json).unwrap_err();
        assert!(err.to_string().contains("BLAKE3 hex"));
    }
}
