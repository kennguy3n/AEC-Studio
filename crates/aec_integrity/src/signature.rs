//! Ed25519-signed manifest verification.
//!
//! ## What's here
//!
//! - [`TrustAnchor`] — a pin of zero or more ed25519 verifying keys
//!   that the binary will trust. The production constant
//!   [`TrustAnchor::EMPTY`] is intentionally empty until the signing
//!   server lands.
//! - [`SignedManifest`] — a JSON-serializable container for a payload
//!   plus a detached ed25519 signature over the canonical bytes of
//!   that payload.
//! - [`verify_manifest`] — checks the signature against any key in
//!   the trust anchor.
//!
//! ## What's *not* here (and why)
//!
//! No private keys, no signing keys, no signing routine in the
//! production code path. The reason is operational: a signing key
//! that lives in a crate the desktop binary links to defeats the
//! purpose of code-signing in the first place. Once a signing server
//! exists, signing will live there (probably as a Rust binary that
//! reuses [`SignedManifest::payload_bytes`] for the canonical
//! serialization, and `ed25519_dalek::SigningKey` for the actual
//! sign), and this crate will continue to be **verify-only**.
//!
//! The test module below *does* construct a [`ed25519_dalek::SigningKey`]
//! and sign with it — that's the only way to exercise the verifier
//! end-to-end. Those signing keys exist solely in the test process
//! memory; they are never serialized and never reach the production
//! binary.

use std::fmt;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Length of an ed25519 signature in bytes.
pub const SIGNED_MANIFEST_SIG_LEN: usize = ed25519_dalek::SIGNATURE_LENGTH;

/// Errors returned by [`TrustAnchor::from_hex_keys`].
#[derive(Debug, Error)]
pub enum TrustAnchorError {
    /// One of the key strings was not the expected 64-hex-character length.
    #[error("ed25519 public key must be 64 hex chars (32 bytes), got {got}")]
    BadLength {
        /// Length of the malformed input.
        got: usize,
    },
    /// One of the key strings was not valid hex.
    #[error("ed25519 public key hex decode failed: {0}")]
    BadHex(String),
    /// The decoded bytes did not form a valid ed25519 public key
    /// (e.g. not on the curve).
    #[error("ed25519 public key bytes are not a valid curve point: {0}")]
    InvalidKey(String),
}

/// A frozen set of ed25519 verifying keys, any of which the binary
/// will trust to sign manifests.
///
/// The production binary calls [`TrustAnchor::production`] which
/// returns [`TrustAnchor::EMPTY`]; once a signing server is online
/// and a key is generated, populating that constant is the only
/// production code change needed (plus a re-release of the binary,
/// which is what makes the trust pin meaningful — a malicious
/// manifest cannot retroactively change the trust set on an already
/// installed binary).
#[derive(Clone, Default)]
pub struct TrustAnchor {
    keys: Vec<VerifyingKey>,
}

impl TrustAnchor {
    /// The empty trust anchor — verification against this anchor
    /// always fails with [`SignatureError::UntrustedKey`].
    ///
    /// This is **intentional** until the signing server is provisioned.
    /// Fail-closed is the correct posture: an empty trust anchor
    /// means "trust no manifest" rather than "trust every manifest".
    pub const EMPTY: Self = Self { keys: Vec::new() };

    /// The trust anchor the production binary uses. Today this is
    /// [`Self::EMPTY`]; when the signing server is provisioned this
    /// will be updated to return the pinned production public keys.
    ///
    /// Splitting this from `EMPTY` keeps the production call site
    /// stable across the eventual key rotation.
    #[must_use]
    pub fn production() -> Self {
        Self::EMPTY
    }

    /// Construct a trust anchor from a list of hex-encoded ed25519
    /// public keys. Each key must be exactly 64 hex characters (32
    /// bytes).
    ///
    /// Useful for tests, for the future signing-server flow that
    /// reads keys from environment / config, and for any out-of-band
    /// key rotation tooling.
    pub fn from_hex_keys<I, S>(keys: I) -> Result<Self, TrustAnchorError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut decoded = Vec::new();
        for key_hex in keys {
            let key_hex = key_hex.as_ref();
            if key_hex.len() != ed25519_dalek::PUBLIC_KEY_LENGTH * 2 {
                return Err(TrustAnchorError::BadLength { got: key_hex.len() });
            }
            let mut bytes = [0u8; ed25519_dalek::PUBLIC_KEY_LENGTH];
            hex::decode_to_slice(key_hex, &mut bytes)
                .map_err(|e| TrustAnchorError::BadHex(e.to_string()))?;
            let vk = VerifyingKey::from_bytes(&bytes)
                .map_err(|e| TrustAnchorError::InvalidKey(e.to_string()))?;
            decoded.push(vk);
        }
        Ok(Self { keys: decoded })
    }

    /// Build a trust anchor directly from raw 32-byte key arrays.
    /// Used by tests and by the future signing server's key-rotation
    /// helpers; the public surface for the desktop binary should
    /// always go through [`Self::from_hex_keys`].
    pub fn from_verifying_keys(keys: Vec<VerifyingKey>) -> Self {
        Self { keys }
    }

    /// Returns `true` if any of the trusted keys produced the given
    /// signature over the given message.
    #[must_use]
    pub fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        self.keys
            .iter()
            .any(|vk| vk.verify(message, signature).is_ok())
    }

    /// Number of keys in the anchor. Useful for the bridge's startup
    /// log line ("integrity anchor: 0 keys (signing infrastructure
    /// not yet provisioned)").
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True if no keys are pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl fmt::Debug for TrustAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustAnchor")
            .field("keys_pinned", &self.keys.len())
            .finish_non_exhaustive()
    }
}

/// Errors returned by [`verify_manifest`].
#[derive(Debug, Error)]
pub enum SignatureError {
    /// The manifest's signature did not match any key in the trust
    /// anchor. This is the failure mode for an empty
    /// [`TrustAnchor::EMPTY`] anchor as well — fail-closed.
    #[error("signature did not match any trusted key (trust anchor has {anchor_key_count} keys)")]
    UntrustedKey {
        /// Number of keys in the anchor (for diagnostics; never zero
        /// if `verify_manifest` was actually invoked with content).
        anchor_key_count: usize,
    },
    /// The signature field was the wrong length / wrong shape.
    #[error("manifest signature is malformed: {0}")]
    MalformedSignature(String),
    /// The manifest could not be serialized to the canonical form
    /// the signature would have been computed over.
    #[error("manifest payload could not be canonicalized: {0}")]
    PayloadCanonicalization(String),
}

/// A JSON-serialized payload + a detached ed25519 signature over its
/// canonical bytes.
///
/// The signature is hex-encoded so the whole thing stays
/// human-inspectable; payload bytes use
/// [`SignedManifest::payload_bytes`] which is `serde_json::to_vec`
/// over the payload (stable JSON output, byte-identical between the
/// signer + verifier as long as both go through `serde_json`).
///
/// `P` is the payload type — typically a struct containing model
/// hashes, version metadata, signing time. The whole manifest is
/// generic over `P` so the same verifier surface can be reused for
/// model-registry manifests, plugin manifests, update manifests, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest<P> {
    /// The signed payload. Type-parameterized so the verifier can be
    /// reused for any JSON-serializable manifest shape.
    pub payload: P,
    /// The detached ed25519 signature, lowercase hex-encoded.
    pub signature_hex: String,
}

impl<P> SignedManifest<P>
where
    P: Serialize,
{
    /// Canonical bytes the signature is computed over. This is
    /// `serde_json::to_vec(&self.payload)`. Both the signer and the
    /// verifier must go through this function for the byte-identical
    /// guarantee.
    pub fn payload_bytes(&self) -> Result<Vec<u8>, SignatureError> {
        serde_json::to_vec(&self.payload)
            .map_err(|e| SignatureError::PayloadCanonicalization(e.to_string()))
    }
}

/// Verify a manifest against a trust anchor. Returns `Ok(())` if any
/// key in the anchor signed the manifest; [`SignatureError`] otherwise.
///
/// This is the entrypoint the bridge will call from
/// `validate_at_boot` once the signing server is online. Today
/// [`TrustAnchor::production`] returns an empty anchor, so calling
/// this with the production anchor on a real manifest cleanly fails
/// with [`SignatureError::UntrustedKey`]. The verification
/// machinery itself is real — the integration tests exercise it
/// against an ad-hoc deterministic test key.
pub fn verify_manifest<P>(
    manifest: &SignedManifest<P>,
    anchor: &TrustAnchor,
) -> Result<(), SignatureError>
where
    P: Serialize,
{
    // Decode the signature hex into the fixed-size ed25519 signature
    // shape. Doing this *before* canonicalizing the payload keeps
    // malformed-signature errors cheap (no JSON serialization on the
    // failure path).
    if manifest.signature_hex.len() != SIGNED_MANIFEST_SIG_LEN * 2 {
        return Err(SignatureError::MalformedSignature(format!(
            "expected {} hex chars, got {}",
            SIGNED_MANIFEST_SIG_LEN * 2,
            manifest.signature_hex.len()
        )));
    }
    let mut sig_bytes = [0u8; SIGNED_MANIFEST_SIG_LEN];
    hex::decode_to_slice(&manifest.signature_hex, &mut sig_bytes)
        .map_err(|e| SignatureError::MalformedSignature(e.to_string()))?;
    let signature = Signature::from_bytes(&sig_bytes);

    let payload_bytes = manifest.payload_bytes()?;
    if anchor.verify(&payload_bytes, &signature) {
        Ok(())
    } else {
        Err(SignatureError::UntrustedKey {
            anchor_key_count: anchor.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde::{Deserialize, Serialize};

    /// A toy payload type that exercises the generic verifier surface.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct TestPayload {
        version: String,
        models: Vec<String>,
    }

    /// Deterministic signing key for tests. NEVER use this outside
    /// `#[cfg(test)]` — it's a constant seed so the test is
    /// reproducible. The corresponding verifying key is derived from
    /// it inside the test.
    const TEST_SIGNING_SEED: [u8; 32] = [42u8; 32];

    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_SIGNING_SEED)
    }

    fn test_payload() -> TestPayload {
        TestPayload {
            version: "1.0.0".to_string(),
            models: vec!["a".to_string(), "b".to_string()],
        }
    }

    fn sign_manifest<P: Serialize>(payload: P, signer: &SigningKey) -> SignedManifest<P> {
        let bytes = serde_json::to_vec(&payload).expect("payload serializes");
        let sig = signer.sign(&bytes);
        SignedManifest {
            payload,
            signature_hex: hex::encode(sig.to_bytes()),
        }
    }

    #[test]
    fn production_trust_anchor_is_empty_until_signing_server_provisioned() {
        let anchor = TrustAnchor::production();
        assert!(
            anchor.is_empty(),
            "production trust anchor must be empty until ed25519 signing keys are provisioned"
        );
        assert_eq!(anchor.len(), 0);
    }

    #[test]
    fn verify_manifest_succeeds_against_trust_anchor_containing_signer_key() {
        let signer = test_signing_key();
        let anchor = TrustAnchor::from_verifying_keys(vec![signer.verifying_key()]);
        let manifest = sign_manifest(test_payload(), &signer);
        verify_manifest(&manifest, &anchor).expect("verify must succeed");
    }

    #[test]
    fn verify_manifest_fails_against_empty_trust_anchor() {
        let signer = test_signing_key();
        let manifest = sign_manifest(test_payload(), &signer);
        let err = verify_manifest(&manifest, &TrustAnchor::EMPTY).unwrap_err();
        match err {
            SignatureError::UntrustedKey { anchor_key_count } => {
                assert_eq!(anchor_key_count, 0);
            }
            other => panic!("expected UntrustedKey, got {other:?}"),
        }
    }

    #[test]
    fn verify_manifest_fails_against_anchor_containing_only_other_keys() {
        let signer = test_signing_key();
        let other_signer = SigningKey::from_bytes(&[7u8; 32]);
        let anchor = TrustAnchor::from_verifying_keys(vec![other_signer.verifying_key()]);
        let manifest = sign_manifest(test_payload(), &signer);
        let err = verify_manifest(&manifest, &anchor).unwrap_err();
        match err {
            SignatureError::UntrustedKey { anchor_key_count } => {
                assert_eq!(anchor_key_count, 1);
            }
            other => panic!("expected UntrustedKey, got {other:?}"),
        }
    }

    #[test]
    fn verify_manifest_succeeds_when_any_key_in_anchor_matches() {
        // Models the "key rotation in progress" state: two valid
        // signing keys, the manifest signed by one of them, anchor
        // contains both.
        let new_signer = test_signing_key();
        let old_signer = SigningKey::from_bytes(&[7u8; 32]);
        let anchor = TrustAnchor::from_verifying_keys(vec![
            old_signer.verifying_key(),
            new_signer.verifying_key(),
        ]);

        let manifest_new = sign_manifest(test_payload(), &new_signer);
        verify_manifest(&manifest_new, &anchor).expect("new-key manifest verifies");

        let manifest_old = sign_manifest(test_payload(), &old_signer);
        verify_manifest(&manifest_old, &anchor).expect("old-key manifest verifies");
    }

    #[test]
    fn verify_manifest_detects_payload_tampering() {
        let signer = test_signing_key();
        let anchor = TrustAnchor::from_verifying_keys(vec![signer.verifying_key()]);
        let mut manifest = sign_manifest(test_payload(), &signer);

        manifest.payload.version = "9.9.9-tampered".to_string();

        let err = verify_manifest(&manifest, &anchor).unwrap_err();
        assert!(matches!(err, SignatureError::UntrustedKey { .. }));
    }

    #[test]
    fn verify_manifest_rejects_malformed_signature_hex_length() {
        let signer = test_signing_key();
        let anchor = TrustAnchor::from_verifying_keys(vec![signer.verifying_key()]);
        let mut manifest = sign_manifest(test_payload(), &signer);
        manifest.signature_hex.truncate(10);
        let err = verify_manifest(&manifest, &anchor).unwrap_err();
        assert!(matches!(err, SignatureError::MalformedSignature(_)));
    }

    #[test]
    fn verify_manifest_rejects_malformed_signature_non_hex_chars() {
        let signer = test_signing_key();
        let anchor = TrustAnchor::from_verifying_keys(vec![signer.verifying_key()]);
        let mut manifest = sign_manifest(test_payload(), &signer);
        // Replace last char with 'z' (not hex). Still 128 chars.
        manifest.signature_hex.pop();
        manifest.signature_hex.push('z');
        let err = verify_manifest(&manifest, &anchor).unwrap_err();
        assert!(matches!(err, SignatureError::MalformedSignature(_)));
    }

    #[test]
    fn trust_anchor_from_hex_keys_round_trip() {
        let signer = test_signing_key();
        let hex_key = hex::encode(signer.verifying_key().to_bytes());
        let anchor = TrustAnchor::from_hex_keys(vec![hex_key.clone()]).unwrap();
        assert_eq!(anchor.len(), 1);

        let manifest = sign_manifest(test_payload(), &signer);
        verify_manifest(&manifest, &anchor).expect("round-tripped key verifies");
    }

    #[test]
    fn trust_anchor_from_hex_keys_rejects_wrong_length() {
        let err = TrustAnchor::from_hex_keys(vec!["deadbeef"]).unwrap_err();
        match err {
            TrustAnchorError::BadLength { got } => assert_eq!(got, 8),
            other => panic!("expected BadLength, got {other:?}"),
        }
    }

    #[test]
    fn trust_anchor_from_hex_keys_rejects_non_hex_chars() {
        // 64 chars, contains 'z'
        let bad_hex = "z".repeat(64);
        let err = TrustAnchor::from_hex_keys(vec![bad_hex]).unwrap_err();
        assert!(matches!(err, TrustAnchorError::BadHex(_)));
    }

    #[test]
    fn signed_manifest_serde_round_trips() {
        let signer = test_signing_key();
        let original = sign_manifest(test_payload(), &signer);
        let json = serde_json::to_string(&original).unwrap();
        let parsed: SignedManifest<TestPayload> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.payload, original.payload);
        assert_eq!(parsed.signature_hex, original.signature_hex);

        let anchor = TrustAnchor::from_verifying_keys(vec![signer.verifying_key()]);
        verify_manifest(&parsed, &anchor).expect("verifies after serde round-trip");
    }
}
