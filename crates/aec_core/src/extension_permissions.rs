//! Manifest validation, permission enforcement, and Ed25519 signature
//! verification for AEC Studio extensions.
//!
//! This module is the policy layer that sits between
//! [`crate::extensions::ExtensionLoader`] and the rest of the workspace:
//!
//! * [`validate_manifest`] runs the structural / format checks the loader
//!   needs *before* a manifest is considered registry-eligible.
//! * [`PermissionEnforcer`] is consulted by every host (asset DB, schedule
//!   registry, AI dispatch, audit log) to gate a runtime operation against
//!   the permissions the extension declared in its manifest.
//! * [`TrustStore`] holds a small set of well-known publisher public keys
//!   that [`crate::extensions::ExtensionLoader`] uses to verify a
//!   manifest's signature.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::extensions::{
    canonical_payload_bytes, ExtensionId, ExtensionManifest, ExtensionSignature, ExtensionType,
    Permission,
};
use crate::types::Scope;

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManifestError {
    #[error("manifest id must not be empty")]
    EmptyId,
    #[error("manifest id must not contain path separators or '..': {id}")]
    UnsafeId { id: String },
    #[error("manifest name must not be empty")]
    EmptyName,
    #[error("manifest version must follow major.minor.patch (got {version})")]
    BadVersion { version: String },
    #[error("manifest license must not be empty")]
    EmptyLicense,
    #[error("permission list contains duplicate entry: {permission}")]
    DuplicatePermission { permission: String },
    #[error("extension declares type {declared} but body is missing or wrong-typed")]
    MissingBody { declared: String },
    #[error("extension declares type {declared} but extra unrelated body is set: {extra}")]
    ExtraBody { declared: String, extra: String },
    #[error("ai tool extension references unknown workflow scope: {scope}")]
    UnknownScope { scope: String },
    #[error("ai tool extension declares max_entities_modified == 0")]
    ZeroMaxEntities,
    #[error("ai tool extension declares empty grammar key")]
    EmptyGrammar,
    #[error("schedule extension declares no columns")]
    EmptyScheduleColumns,
    #[error("schedule formula has empty expression: {formula}")]
    EmptyFormula { formula: String },
    #[error("export target has empty target id")]
    EmptyTargetId,
    #[error("importer declares no file extensions")]
    EmptyImporterExtensions,
    #[error("importer extension entry is empty or contains a leading dot: {ext}")]
    BadImporterExtension { ext: String },
    #[error("asset pack contains entry with empty asset_id")]
    EmptyAssetId,
}

/// Run every structural check the loader cares about. Returning an empty
/// `Vec` means the manifest is *structurally* valid — host-specific wiring
/// (asset file presence, template JSON validity, grammar lookup) is the
/// host's responsibility.
pub fn validate_manifest(m: &ExtensionManifest) -> Vec<ManifestError> {
    let mut errors = Vec::new();

    if m.id.0.is_empty() {
        errors.push(ManifestError::EmptyId);
    } else if m.id.0.contains('/') || m.id.0.contains('\\') || m.id.0.contains("..") {
        errors.push(ManifestError::UnsafeId {
            id: m.id.0.clone(),
        });
    }
    if m.name.is_empty() {
        errors.push(ManifestError::EmptyName);
    }
    if !looks_like_semver(&m.version) {
        errors.push(ManifestError::BadVersion {
            version: m.version.clone(),
        });
    }
    if m.license.is_empty() {
        errors.push(ManifestError::EmptyLicense);
    }

    let mut seen: BTreeSet<Permission> = BTreeSet::new();
    for p in &m.permissions {
        if !seen.insert(*p) {
            errors.push(ManifestError::DuplicatePermission {
                permission: p.as_str().to_string(),
            });
        }
    }

    // Exactly-one-body invariant
    let body_set = (
        m.asset_pack.is_some(),
        m.template.is_some(),
        m.schedule.is_some(),
        m.export_target.is_some(),
        m.ai_tool.is_some(),
        m.importer.is_some(),
    );
    let declared = m.kind;
    let required_present = match declared {
        ExtensionType::AssetPack => body_set.0,
        ExtensionType::Template => body_set.1,
        ExtensionType::Schedule => body_set.2,
        ExtensionType::ExportTarget => body_set.3,
        ExtensionType::AiTool => body_set.4,
        ExtensionType::Importer => body_set.5,
    };
    if !required_present {
        errors.push(ManifestError::MissingBody {
            declared: declared.as_str().to_string(),
        });
    }
    let extras = [
        ("asset_pack", body_set.0, ExtensionType::AssetPack),
        ("template", body_set.1, ExtensionType::Template),
        ("schedule", body_set.2, ExtensionType::Schedule),
        ("export_target", body_set.3, ExtensionType::ExportTarget),
        ("ai_tool", body_set.4, ExtensionType::AiTool),
        ("importer", body_set.5, ExtensionType::Importer),
    ];
    for (name, present, kind) in extras {
        if present && kind != declared {
            errors.push(ManifestError::ExtraBody {
                declared: declared.as_str().to_string(),
                extra: name.to_string(),
            });
        }
    }

    // Per-body checks.
    if let Some(ai) = &m.ai_tool {
        if ai.max_entities_modified == 0 {
            errors.push(ManifestError::ZeroMaxEntities);
        }
        if ai.grammar_key.trim().is_empty() {
            errors.push(ManifestError::EmptyGrammar);
        }
        for s in &ai.allowed_scopes {
            if parse_scope(s).is_none() {
                errors.push(ManifestError::UnknownScope { scope: s.clone() });
            }
        }
    }
    if let Some(sched) = &m.schedule {
        if sched.columns.is_empty() {
            errors.push(ManifestError::EmptyScheduleColumns);
        }
        for f in &sched.formulas {
            if f.expression.trim().is_empty() {
                errors.push(ManifestError::EmptyFormula {
                    formula: f.name.clone(),
                });
            }
        }
    }
    if let Some(et) = &m.export_target {
        if et.target_id.trim().is_empty() {
            errors.push(ManifestError::EmptyTargetId);
        }
    }
    if let Some(imp) = &m.importer {
        if imp.extensions.is_empty() {
            errors.push(ManifestError::EmptyImporterExtensions);
        }
        for ext in &imp.extensions {
            if ext.is_empty() || ext.starts_with('.') {
                errors.push(ManifestError::BadImporterExtension { ext: ext.clone() });
            }
        }
    }
    if let Some(ap) = &m.asset_pack {
        for e in &ap.entries {
            if e.asset_id.trim().is_empty() {
                errors.push(ManifestError::EmptyAssetId);
            }
        }
    }

    errors
}

fn looks_like_semver(v: &str) -> bool {
    // Cheap, dependency-free semver check that accepts `X.Y.Z` with
    // optional `-prerelease` and `+build` suffixes. Each of the three
    // core segments must be a non-empty digit run.
    let core_end = v.find(|c| c == '-' || c == '+').unwrap_or(v.len());
    let core = &v[..core_end];
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn parse_scope(s: &str) -> Option<Scope> {
    match s {
        "design" => Some(Scope::Design),
        "draft" => Some(Scope::Draft),
        "bim" => Some(Scope::Bim),
        "render" => Some(Scope::Render),
        "deliver" => Some(Scope::Deliver),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Permission enforcement
// ---------------------------------------------------------------------------

/// Operations a host crate gates through the enforcer. New variants extend
/// the set without breaking on-disk manifests because permission strings
/// in the manifest are decoupled from the operation kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    ReadGeometry,
    WriteGeometry,
    /// `scope` is a free-form host hint (e.g. `"project_dir"`,
    /// `"user_assets"`) used for audit-log entries; the gate only cares
    /// about the [`Permission::FilesystemWrite`] declaration itself.
    WriteFile { scope: String },
    ReadFile { scope: String },
    UseAiTool,
    WriteAuditLog,
    NetworkAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionCheck {
    AllowedRead,
    AllowedWrite,
    Denied { reason: String },
}

impl PermissionCheck {
    pub fn allowed(&self) -> bool {
        !matches!(self, PermissionCheck::Denied { .. })
    }
}

#[derive(Debug, Clone, Default)]
pub struct PermissionEnforcer {
    /// Per-extension permission set. Constructed once from a registry
    /// snapshot; the enforcer never reads from disk so it is cheap to
    /// share between threads under `Arc`.
    grants: std::collections::BTreeMap<ExtensionId, BTreeSet<Permission>>,
}

impl PermissionEnforcer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(&mut self, id: ExtensionId, permissions: impl IntoIterator<Item = Permission>) {
        self.grants
            .insert(id, permissions.into_iter().collect::<BTreeSet<_>>());
    }

    /// Build an enforcer from a [`crate::extensions::ExtensionRegistry`].
    pub fn from_registry(registry: &crate::extensions::ExtensionRegistry) -> Self {
        let mut enforcer = Self::new();
        for ext in registry.iter() {
            enforcer.grant(
                ext.manifest.id.clone(),
                ext.manifest.permissions.iter().copied(),
            );
        }
        enforcer
    }

    pub fn check_permission(&self, id: &ExtensionId, op: &Operation) -> PermissionCheck {
        let Some(grants) = self.grants.get(id) else {
            return PermissionCheck::Denied {
                reason: format!("extension {id} is not registered with the enforcer"),
            };
        };
        match op {
            Operation::ReadGeometry => {
                if grants.contains(&Permission::GeometryRead) {
                    PermissionCheck::AllowedRead
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare geometry_read".into(),
                    }
                }
            }
            Operation::WriteGeometry => {
                if grants.contains(&Permission::GeometryWrite) {
                    PermissionCheck::AllowedWrite
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare geometry_write".into(),
                    }
                }
            }
            Operation::WriteFile { scope: _ } => {
                if grants.contains(&Permission::FilesystemWrite) {
                    PermissionCheck::AllowedWrite
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare filesystem_write".into(),
                    }
                }
            }
            Operation::ReadFile { scope: _ } => {
                if grants.contains(&Permission::FilesystemRead) {
                    PermissionCheck::AllowedRead
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare filesystem_read".into(),
                    }
                }
            }
            Operation::UseAiTool => {
                if grants.contains(&Permission::AiTools) {
                    PermissionCheck::AllowedWrite
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare ai_tools".into(),
                    }
                }
            }
            Operation::WriteAuditLog => {
                if grants.contains(&Permission::AuditLog) {
                    PermissionCheck::AllowedWrite
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare audit_log".into(),
                    }
                }
            }
            Operation::NetworkAccess => {
                if grants.contains(&Permission::Network) {
                    PermissionCheck::AllowedWrite
                } else {
                    PermissionCheck::Denied {
                        reason: "extension did not declare network".into(),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Signature verification
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("missing signature")]
    MissingSignature,
    #[error("unsupported signature algorithm: {0}")]
    UnsupportedAlgorithm(String),
    #[error("public key hex must be 64 chars (got {0})")]
    BadPublicKeyLength(usize),
    #[error("signature hex must be 128 chars (got {0})")]
    BadSignatureLength(usize),
    #[error("hex decode error: {0}")]
    HexDecode(#[from] hex::FromHexError),
    #[error("invalid ed25519 public key: {0}")]
    BadPublicKey(String),
    #[error("ed25519 signature verification failed")]
    VerifyFailed,
    #[error("public key not present in trust store")]
    UntrustedKey,
}

#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    keys: BTreeSet<[u8; PUBLIC_KEY_LENGTH]>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single_from_hex(hex_key: &str) -> Result<Self, SignatureError> {
        let mut store = Self::new();
        store.add_hex(hex_key)?;
        Ok(store)
    }

    pub fn add_hex(&mut self, hex_key: &str) -> Result<(), SignatureError> {
        let bytes = decode_pk_hex(hex_key)?;
        self.keys.insert(bytes);
        Ok(())
    }

    pub fn contains(&self, key: &[u8; PUBLIC_KEY_LENGTH]) -> bool {
        self.keys.contains(key)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn decode_pk_hex(s: &str) -> Result<[u8; PUBLIC_KEY_LENGTH], SignatureError> {
    if s.len() != PUBLIC_KEY_LENGTH * 2 {
        return Err(SignatureError::BadPublicKeyLength(s.len()));
    }
    let bytes = hex::decode(s)?;
    let mut out = [0u8; PUBLIC_KEY_LENGTH];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_sig_hex(s: &str) -> Result<[u8; SIGNATURE_LENGTH], SignatureError> {
    if s.len() != SIGNATURE_LENGTH * 2 {
        return Err(SignatureError::BadSignatureLength(s.len()));
    }
    let bytes = hex::decode(s)?;
    let mut out = [0u8; SIGNATURE_LENGTH];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Verify the manifest's signature against the embedded public key *and*
/// confirm that public key is present in the supplied trust store.
pub fn verify_signature_against(
    manifest: &ExtensionManifest,
    signature: &ExtensionSignature,
    trust: &TrustStore,
) -> Result<(), SignatureError> {
    if signature.algorithm != "ed25519" {
        return Err(SignatureError::UnsupportedAlgorithm(
            signature.algorithm.clone(),
        ));
    }
    let pk_bytes = decode_pk_hex(&signature.public_key_hex)?;
    if !trust.contains(&pk_bytes) {
        return Err(SignatureError::UntrustedKey);
    }
    let pk = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| SignatureError::BadPublicKey(e.to_string()))?;
    let sig_bytes = decode_sig_hex(&signature.signature_hex)?;
    let sig = Signature::from_bytes(&sig_bytes);
    let payload = canonical_payload_bytes(manifest);
    pk.verify(&payload, &sig)
        .map_err(|_| SignatureError::VerifyFailed)
}

/// Equivalent to [`verify_signature_against`] but without a trust store —
/// useful for development and for the loader's "self-consistency" check
/// when no trust store is configured.
pub fn verify_signature_self_consistent(
    manifest: &ExtensionManifest,
    signature: &ExtensionSignature,
) -> Result<(), SignatureError> {
    let trust = TrustStore::single_from_hex(&signature.public_key_hex)?;
    verify_signature_against(manifest, signature, &trust)
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Generate a fresh Ed25519 keypair. Used by tests in this crate and in
/// host crates. Exposed (rather than gated behind `#[cfg(test)]`) because
/// integration tests in *other* crates need to drive it too.
///
/// Uses `getrandom` (already a dependency of `aec_core`) to fill the
/// 32-byte seed so the helper costs nothing extra at build time and
/// stays available to downstream test suites without pulling in a
/// `rand`-stack `dev-dependency`.
#[doc(hidden)]
pub fn keygen_test_only() -> (ed25519_dalek::SigningKey, String) {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("OS RNG must be available");
    let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
    let pk_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pk_hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{
        AiToolBody, AssetEntry, AssetEntryKind, AssetPackBody, ExtensionManifest, ExtensionType,
        ImporterBody, Permission, ScheduleBody, ScheduleColumnDef, ScheduleValueType,
    };
    use ed25519_dalek::Signer;
    use std::path::PathBuf;

    fn minimal(kind: ExtensionType) -> ExtensionManifest {
        let mut m = ExtensionManifest {
            id: ExtensionId("ext".into()),
            name: "Ext".into(),
            version: "0.1.0".into(),
            kind,
            permissions: vec![],
            signature: None,
            license: "AGPL-3.0".into(),
            description: String::new(),
            asset_pack: None,
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: None,
            importer: None,
        };
        match kind {
            ExtensionType::AssetPack => {
                m.asset_pack = Some(AssetPackBody {
                    vendor: "V".into(),
                    entries: vec![AssetEntry {
                        asset_id: "a1".into(),
                        name: "A1".into(),
                        kind: AssetEntryKind::Furniture,
                        tags: vec![],
                        source_path: PathBuf::from("a/b.glb"),
                        blake3: String::new(),
                    }],
                });
            }
            ExtensionType::Template => {
                m.template = Some(crate::extensions::TemplateBody {
                    key: "interior.x".into(),
                    definition_path: PathBuf::from("template.json"),
                });
            }
            ExtensionType::Schedule => {
                m.schedule = Some(ScheduleBody {
                    schedule_id: "ext.sched".into(),
                    display_name: "Sched".into(),
                    columns: vec![ScheduleColumnDef {
                        key: "k".into(),
                        header: "K".into(),
                        value_type: ScheduleValueType::String,
                        default: None,
                    }],
                    formulas: vec![],
                });
            }
            ExtensionType::ExportTarget => {
                m.export_target = Some(crate::extensions::ExportTargetBody {
                    target_id: "ext.tgt".into(),
                    display_name: "Tgt".into(),
                    format: crate::extensions::ExportFormat::Pdf,
                    entry_path: PathBuf::from("export.json"),
                    default_extension: "pdf".into(),
                });
            }
            ExtensionType::AiTool => {
                m.ai_tool = Some(AiToolBody {
                    tool_id: "ext.tool".into(),
                    display_name: "Tool".into(),
                    description: String::new(),
                    allowed_scopes: vec!["design".into()],
                    max_entities_modified: 16,
                    grammar_key: "tool_grammar".into(),
                });
            }
            ExtensionType::Importer => {
                m.importer = Some(ImporterBody {
                    importer_id: "ext.imp".into(),
                    display_name: "Imp".into(),
                    extensions: vec!["skp".into()],
                    entry_path: PathBuf::from("imp.json"),
                });
            }
        }
        m
    }

    #[test]
    fn valid_manifests_produce_no_errors() {
        for k in ExtensionType::ALL {
            let m = minimal(*k);
            assert!(
                validate_manifest(&m).is_empty(),
                "type {:?} should be valid",
                k
            );
        }
    }

    #[test]
    fn empty_required_fields_each_emit_a_specific_error() {
        let mut m = minimal(ExtensionType::AssetPack);
        m.name = String::new();
        m.version = "x.y.z".into();
        m.license = String::new();
        m.id = ExtensionId(String::new());
        let errors = validate_manifest(&m);
        assert!(errors.contains(&ManifestError::EmptyName));
        assert!(errors.contains(&ManifestError::EmptyLicense));
        assert!(errors.contains(&ManifestError::EmptyId));
        assert!(errors
            .iter()
            .any(|e| matches!(e, ManifestError::BadVersion { .. })));
    }

    #[test]
    fn wrong_body_for_type_is_rejected() {
        let mut m = minimal(ExtensionType::AssetPack);
        m.template = Some(crate::extensions::TemplateBody {
            key: "x.y".into(),
            definition_path: PathBuf::from("t.json"),
        });
        let errors = validate_manifest(&m);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ManifestError::ExtraBody { .. })));
    }

    #[test]
    fn ai_tool_scope_and_grammar_validation() {
        let mut m = minimal(ExtensionType::AiTool);
        if let Some(ai) = m.ai_tool.as_mut() {
            ai.allowed_scopes = vec!["render".into(), "nope".into()];
            ai.max_entities_modified = 0;
            ai.grammar_key = String::new();
        }
        let errors = validate_manifest(&m);
        assert!(errors.contains(&ManifestError::UnknownScope {
            scope: "nope".into()
        }));
        assert!(errors.contains(&ManifestError::ZeroMaxEntities));
        assert!(errors.contains(&ManifestError::EmptyGrammar));
    }

    #[test]
    fn duplicate_permission_is_an_error() {
        let mut m = minimal(ExtensionType::AssetPack);
        m.permissions = vec![Permission::FilesystemRead, Permission::FilesystemRead];
        let errors = validate_manifest(&m);
        assert!(errors.contains(&ManifestError::DuplicatePermission {
            permission: "filesystem_read".into()
        }));
    }

    #[test]
    fn enforcer_denies_undeclared_permissions() {
        let mut m = minimal(ExtensionType::AiTool);
        if let Some(ai) = m.ai_tool.as_mut() {
            ai.allowed_scopes = vec!["design".into()];
        }
        m.permissions = vec![Permission::GeometryRead];
        let mut enf = PermissionEnforcer::new();
        enf.grant(m.id.clone(), m.permissions.iter().copied());
        // declared:
        assert!(enf
            .check_permission(&m.id, &Operation::ReadGeometry)
            .allowed());
        // undeclared:
        assert!(!enf
            .check_permission(&m.id, &Operation::WriteGeometry)
            .allowed());
        assert!(!enf.check_permission(&m.id, &Operation::UseAiTool).allowed());
        assert!(!enf
            .check_permission(&m.id, &Operation::NetworkAccess)
            .allowed());
    }

    #[test]
    fn enforcer_rejects_unknown_extension() {
        let enf = PermissionEnforcer::new();
        let check = enf.check_permission(&ExtensionId("missing".into()), &Operation::UseAiTool);
        assert!(!check.allowed());
    }

    #[test]
    fn signature_verifies_only_when_payload_matches() {
        let (sk, pk_hex) = keygen_test_only();
        let mut m = minimal(ExtensionType::AssetPack);
        let payload = canonical_payload_bytes(&m);
        let sig = sk.sign(&payload);
        m.signature = Some(ExtensionSignature {
            algorithm: "ed25519".into(),
            public_key_hex: pk_hex.clone(),
            signature_hex: hex::encode(sig.to_bytes()),
        });
        let trust = TrustStore::single_from_hex(&pk_hex).unwrap();
        verify_signature_against(&m, m.signature.as_ref().unwrap(), &trust).unwrap();

        // Tamper with the name → signature must fail.
        let mut tampered = m.clone();
        tampered.name = "evil".into();
        let err = verify_signature_against(&tampered, m.signature.as_ref().unwrap(), &trust)
            .unwrap_err();
        assert!(matches!(err, SignatureError::VerifyFailed));
    }

    #[test]
    fn untrusted_key_fails_verification() {
        let (sk, _pk_hex) = keygen_test_only();
        let mut m = minimal(ExtensionType::AssetPack);
        let payload = canonical_payload_bytes(&m);
        let sig = sk.sign(&payload);
        m.signature = Some(ExtensionSignature {
            algorithm: "ed25519".into(),
            public_key_hex: hex::encode(sk.verifying_key().to_bytes()),
            signature_hex: hex::encode(sig.to_bytes()),
        });
        let other = TrustStore::single_from_hex(&hex::encode(
            ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng)
                .verifying_key()
                .to_bytes(),
        ))
        .unwrap();
        let err =
            verify_signature_against(&m, m.signature.as_ref().unwrap(), &other).unwrap_err();
        assert!(matches!(err, SignatureError::UntrustedKey));
    }

    #[test]
    fn unsupported_algorithm_is_rejected() {
        let (sk, pk_hex) = keygen_test_only();
        let m = minimal(ExtensionType::AssetPack);
        let payload = canonical_payload_bytes(&m);
        let sig = sk.sign(&payload);
        let bad_alg = ExtensionSignature {
            algorithm: "secp256k1".into(),
            public_key_hex: pk_hex.clone(),
            signature_hex: hex::encode(sig.to_bytes()),
        };
        let trust = TrustStore::single_from_hex(&pk_hex).unwrap();
        let err = verify_signature_against(&m, &bad_alg, &trust).unwrap_err();
        assert!(matches!(err, SignatureError::UnsupportedAlgorithm(_)));
    }
}
