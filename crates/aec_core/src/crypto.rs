//! BLAKE3 content hashing and per-project key derivation.
//!
//! AEC Studio uses BLAKE3 for content-addressed dedup of geometry, assets,
//! and the audit/command logs. Per-project encryption keys are derived from
//! a (master_key, project_nonce) pair using BLAKE3's keyed-hash mode. This
//! mirrors the cryptographic-forgetting pattern used in
//! [`kennguy3n/knowledge`](https://github.com/kennguy3n/knowledge): deleting
//! the nonce permanently invalidates the project.

use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// 32-byte key wrapper used both for SQLCipher and for derivation contexts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key32(#[serde(with = "hex_bytes")] pub [u8; 32]);

impl Key32 {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            serde::de::Error::custom(format!("expected 32 bytes, got {}", v.len()))
        })?;
        Ok(arr)
    }
}

/// Compute the BLAKE3 hex digest of a byte slice.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Compute the BLAKE3 prefix-tagged digest (`blake3:<hex>`).
pub fn blake3_tag(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

/// Derive a per-project Key32 from a master key and a project nonce.
///
/// The derivation context is `"aec-studio/project-key/v1"`. The nonce is fed
/// into the keyed hash; deleting the nonce destroys the key (crypto-forget).
pub fn derive_project_key(master_key: &[u8; 32], project_nonce: &[u8]) -> Key32 {
    let mut hasher = Hasher::new_keyed(master_key);
    hasher.update(b"aec-studio/project-key/v1");
    hasher.update(project_nonce);
    let hash = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    Key32(out)
}

/// Generate a fresh random nonce for a new project.
///
/// Sourced directly from the OS CSPRNG via [`getrandom::getrandom`]. We
/// used to derive the nonce by concatenating two `Uuid::new_v4` values
/// (each backed by `getrandom` internally), but that path donated four
/// bits per UUID to version/variant tagging — full bytes from
/// `getrandom` are both cleaner and architecturally identical to what
/// every other AEC Studio crypto primitive expects.
///
/// # Errors
///
/// Returns [`getrandom::Error`] if the OS RNG is unavailable (extremely
/// rare; only happens on broken or pre-init kernels). Callers in the
/// project-package layer surface this through [`crate::error::AecError`].
pub fn generate_project_nonce() -> Result<[u8; 32], getrandom::Error> {
    let mut out = [0u8; 32];
    getrandom::getrandom(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_is_deterministic() {
        let a = blake3_hex(b"hello");
        let b = blake3_hex(b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn blake3_differs_for_distinct_inputs() {
        let a = blake3_hex(b"hello");
        let b = blake3_hex(b"world");
        assert_ne!(a, b);
    }

    #[test]
    fn blake3_tag_has_prefix() {
        let t = blake3_tag(b"x");
        assert!(t.starts_with("blake3:"));
        assert_eq!(t.len(), "blake3:".len() + 64);
    }

    #[test]
    fn derive_project_key_is_deterministic() {
        let master = [7u8; 32];
        let nonce = [9u8; 32];
        let a = derive_project_key(&master, &nonce);
        let b = derive_project_key(&master, &nonce);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_project_key_differs_on_nonce() {
        let master = [7u8; 32];
        let a = derive_project_key(&master, &[1u8; 32]);
        let b = derive_project_key(&master, &[2u8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn generate_project_nonce_is_unique() {
        let n1 = generate_project_nonce().unwrap();
        let n2 = generate_project_nonce().unwrap();
        assert_ne!(n1, n2);
    }

    #[test]
    fn generate_project_nonce_has_high_entropy() {
        // Spot-check that we're not getting a zero buffer / single repeated
        // byte. `getrandom` is supposed to fill all 32 bytes with CSPRNG
        // output, so we expect at least a handful of distinct values.
        let n = generate_project_nonce().unwrap();
        let distinct = n.iter().copied().collect::<std::collections::HashSet<_>>();
        assert!(
            distinct.len() >= 8,
            "nonce had suspiciously low byte diversity: {n:?}"
        );
    }

    #[test]
    fn key32_serde_roundtrip() {
        let k = Key32([42u8; 32]);
        let j = serde_json::to_string(&k).unwrap();
        let back: Key32 = serde_json::from_str(&j).unwrap();
        assert_eq!(k, back);
    }
}
