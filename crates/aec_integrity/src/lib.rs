//! # `aec_integrity` — production binary + model integrity verification.
//!
//! Phase 18 Group E Task 24. This crate isolates the verification
//! machinery that lets the AEC Studio bridge decide, at boot, whether
//! the model files on disk are the bytes a code-signed binary expects.
//! It has no dependency on `aec_ai`, `aec_bridge`, or any of the
//! runtime crates so the verification layer can be unit-tested in
//! isolation and reused by future surfaces (signed model manifests,
//! signed update artifacts, signed plugin bundles).
//!
//! ## Layered design
//!
//! The chain of trust the user actually experiences has three layers:
//!
//! 1. **Code-signed installer / binary.** Apple Developer ID notarization
//!    on macOS, Authenticode on Windows. This is enforced by
//!    `electron-builder` and is out of scope for this crate.
//! 2. **Compile-time BLAKE3 pinning** of every model the registry
//!    knows about. The hashes live in
//!    `crates/aec_ai/data/ai_models.json` and are baked into the
//!    code-signed binary via `include_str!` — there is no
//!    network-fetched manifest, by design. [`Blake3Digest`] is the
//!    typed wrapper this crate exposes for those hashes.
//! 3. **Boot-time file verification.** When the bridge starts it
//!    walks the model directory, computes BLAKE3 over every
//!    descriptor-pinned file, and compares it against the
//!    compile-time hash. Mismatches are quarantined (Task 26 wires
//!    the deletion + warn path).
//!
//! For a future layer 4 — an **ed25519-signed update manifest** —
//! the [`SignedManifest`] / [`TrustAnchor`] / [`verify_manifest`]
//! surface is the API the bridge will use. Today the production
//! [`TrustAnchor::EMPTY`] is empty (we don't yet operate a signing
//! server), so verification cleanly fails closed with
//! [`SignatureError::UntrustedKey`]; the verification machinery
//! itself is **real working code** that can sign + verify against
//! ad-hoc keypairs (the integration tests exercise this path with a
//! deterministic test key). When the signing server lands, populating
//! the production anchor is a one-line constant change.
//!
//! ## Why a separate crate?
//!
//! - **Reusability.** The bridge, the future update channel, and any
//!   plugin loader can all call this without dragging in `aec_ai`'s
//!   HTTP + sidecar stack.
//! - **Clean dependency boundary.** This crate depends on
//!   `blake3 + ed25519-dalek + serde + serde_json + thiserror +
//!   tracing` and nothing else — meaning it cannot accidentally pull
//!   in a runtime dependency that would expand the trust surface.
//! - **No-Python invariant** (Task 23). The compile-time BLAKE3 chain
//!   is what makes the no-Python invariant detectable at boot — if a
//!   future maintainer ever pulls a `gemlite` / `mlx` / `hqq` model
//!   into the registry, the [`ModelVerificationOutcome::Mismatch`]
//!   path will fire on every machine and force the issue.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod blake3_digest;
mod boot;
mod signature;

pub use blake3_digest::{
    blake3_file, Blake3Digest, Blake3DigestError, BLAKE3_HEX_LEN, BLAKE3_STREAM_CHUNK_BYTES,
};
pub use boot::{
    verify_files_against_pins, FilePin, ModelVerificationOutcome, ModelVerificationStatus,
    VerificationReport, VerificationReportEntry, VerifyFilesError,
};
pub use signature::{
    verify_manifest, SignatureError, SignedManifest, TrustAnchor, TrustAnchorError,
    SIGNED_MANIFEST_SIG_LEN,
};
