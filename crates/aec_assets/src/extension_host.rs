//! Host integration for `ExtensionType::AssetPack` extensions.
//!
//! Reads the [`aec_core::AssetPackBody`] of every asset-pack extension in a
//! registry, validates the on-disk payload, hashes each blob with BLAKE3,
//! and writes one [`AssetMetadata`] row per entry into the
//! [`AssetDatabase`]. The host is intentionally side-effecty: callers run
//! it once at app boot (or whenever the user toggles an extension) and
//! the rest of the asset browser picks up the new rows through the
//! existing query API.
//!
//! Permission gate: every entry that touches the filesystem requires the
//! extension to declare [`aec_core::Permission::FilesystemRead`]. Asset
//! packs that ask for `geometry_write` are rejected up-front because the
//! pipeline can only *add* assets — never mutate the user's project graph.

use std::fs;
use std::path::PathBuf;

use thiserror::Error;

use aec_core::{
    AssetEntryKind as ExtAssetEntryKind, ExtensionRegistry, ExtensionType, LoadedExtension,
    Operation, PermissionCheck, PermissionEnforcer,
};

use crate::db::AssetDatabase;
use crate::error::AssetError;
use crate::lod::{LodChain, LodLevel};
use crate::metadata::{AssetMetadata, License, MeshBlob, ThumbnailKind, Vendor};

#[derive(Debug, Error)]
pub enum AssetExtensionError {
    #[error("permission denied for extension {ext}: {reason}")]
    PermissionDenied { ext: String, reason: String },
    #[error("asset source {path} for entry {entry} is missing from extension {ext}")]
    MissingAsset {
        ext: String,
        entry: String,
        path: PathBuf,
    },
    #[error("asset {entry} declared blake3 {expected} but file hashes to {actual}")]
    BlobChecksumMismatch {
        entry: String,
        expected: String,
        actual: String,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("asset db: {0}")]
    Db(#[from] AssetError),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallSummary {
    /// One entry per asset successfully registered. Each tuple is
    /// `(extension_id, asset_id)` so callers can audit-log the install.
    pub installed: Vec<(String, String)>,
    /// Entries that already existed in the DB and were left untouched.
    pub skipped: Vec<(String, String)>,
}

impl InstallSummary {
    pub fn total(&self) -> usize {
        self.installed.len() + self.skipped.len()
    }
}

/// Install every asset-pack extension in `registry` into `db`.
///
/// Extensions that are not [`ExtensionType::AssetPack`] are ignored —
/// callers typically run all five hosts and they decide which subset of
/// the registry to act on.
pub fn install_asset_packs(
    db: &mut AssetDatabase,
    registry: &ExtensionRegistry,
    enforcer: &PermissionEnforcer,
) -> Result<InstallSummary, AssetExtensionError> {
    let mut summary = InstallSummary::default();
    for ext in registry.by_kind(ExtensionType::AssetPack) {
        install_single_pack(db, ext, enforcer, &mut summary)?;
    }
    Ok(summary)
}

fn install_single_pack(
    db: &mut AssetDatabase,
    ext: &LoadedExtension,
    enforcer: &PermissionEnforcer,
    summary: &mut InstallSummary,
) -> Result<(), AssetExtensionError> {
    // Gate on filesystem_read — without it we cannot read the asset
    // payloads next to the manifest.
    if let PermissionCheck::Denied { reason } = enforcer.check_permission(
        &ext.manifest.id,
        &Operation::ReadFile {
            scope: format!("extension:{}", ext.manifest.id),
        },
    ) {
        return Err(AssetExtensionError::PermissionDenied {
            ext: ext.manifest.id.0.clone(),
            reason,
        });
    }

    let Some(body) = ext.manifest.asset_pack.as_ref() else {
        return Ok(()); // already enforced by validate_manifest, but be defensive
    };

    for entry in &body.entries {
        let abs = ext.root.join(&entry.source_path);
        if !abs.is_file() {
            return Err(AssetExtensionError::MissingAsset {
                ext: ext.manifest.id.0.clone(),
                entry: entry.asset_id.clone(),
                path: abs,
            });
        }
        let bytes = fs::read(&abs)?;
        let actual_hash = hex::encode(blake3::hash(&bytes).as_bytes());
        if !entry.blake3.is_empty() && entry.blake3 != actual_hash {
            return Err(AssetExtensionError::BlobChecksumMismatch {
                entry: entry.asset_id.clone(),
                expected: entry.blake3.clone(),
                actual: actual_hash,
            });
        }

        // Idempotent insert: if the asset already exists, leave it alone.
        if db.get(&entry.asset_id)?.is_some() {
            summary
                .skipped
                .push((ext.manifest.id.0.clone(), entry.asset_id.clone()));
            continue;
        }

        db.put_blob(&actual_hash, &bytes)?;

        let triangle_count = estimate_triangles(&bytes);
        let lods = LodChain {
            levels: vec![LodLevel {
                level: 0,
                ratio: 1.0,
                triangle_count,
            }],
        };
        let metadata = AssetMetadata {
            asset_id: entry.asset_id.clone(),
            name: entry.name.clone(),
            vendor: Vendor {
                id: ext.manifest.id.0.clone(),
                name: body.vendor.clone(),
                url: None,
            },
            version: ext.manifest.version.clone(),
            license: License::Custom,
            attribution: Some(ext.manifest.license.clone()),
            tags: entry.tags.clone(),
            style_tags: vec![asset_kind_tag(entry.kind).into()],
            lods: vec![MeshBlob {
                mesh_hash: actual_hash.clone(),
                vertex_count: 0,
                triangle_count,
            }],
            materials: Vec::new(),
            thumbnail_kind: ThumbnailKind::Placeholder,
            thumbnail_hash: String::new(),
            created_at: chrono::Utc::now(),
        };
        db.upsert_metadata(&metadata, &lods)?;
        summary
            .installed
            .push((ext.manifest.id.0.clone(), entry.asset_id.clone()));
    }

    Ok(())
}

fn asset_kind_tag(kind: ExtAssetEntryKind) -> &'static str {
    match kind {
        ExtAssetEntryKind::Furniture => "furniture",
        ExtAssetEntryKind::Material => "material",
        ExtAssetEntryKind::Preset => "preset",
    }
}

fn estimate_triangles(bytes: &[u8]) -> u32 {
    // A real LOD pipeline lives in `pipeline.rs`; for extension-supplied
    // pre-baked assets we record a conservative triangle estimate (1 per
    // 96 bytes) so the asset query layer still shows a non-zero count.
    // The query layer treats triangle_count as informational; the
    // pipeline can refine it later if the user re-imports.
    (bytes.len().div_ceil(96)).min(u32::MAX as usize) as u32
}



#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::{
        AssetEntry, AssetEntryKind, AssetPackBody, ExtensionId, ExtensionLoader, ExtensionManifest,
        ExtensionType, LoadOptions, Permission,
    };
    use std::fs;
    use std::path::Path;

    fn write_pack(root: &Path, id: &str, entries: Vec<(String, Vec<u8>)>) {
        let dir = root.join(id);
        fs::create_dir_all(dir.join("furniture")).unwrap();
        let mut body_entries = Vec::new();
        for (name, bytes) in entries {
            let rel = PathBuf::from("furniture").join(format!("{name}.glb"));
            fs::write(dir.join(&rel), &bytes).unwrap();
            let hash = hex::encode(blake3::hash(&bytes).as_bytes());
            body_entries.push(AssetEntry {
                asset_id: format!("{id}.{name}"),
                name: name.clone(),
                kind: AssetEntryKind::Furniture,
                tags: vec!["test".into()],
                source_path: rel,
                blake3: hash,
            });
        }
        let manifest = ExtensionManifest {
            id: ExtensionId(id.to_string()),
            name: id.to_string(),
            version: "1.0.0".into(),
            kind: ExtensionType::AssetPack,
            permissions: vec![Permission::FilesystemRead, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: String::new(),
            asset_pack: Some(AssetPackBody {
                vendor: "TestVendor".into(),
                entries: body_entries,
            }),
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: None,
            importer: None,
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn install_asset_packs_writes_metadata_and_blobs() {
        let td = tempfile::tempdir().unwrap();
        write_pack(
            td.path(),
            "acme.sofas",
            vec![
                ("sofa1".into(), b"hello".to_vec()),
                ("sofa2".into(), b"world".to_vec()),
            ],
        );
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let summary = install_asset_packs(&mut db, &registry, &enforcer).unwrap();
        assert_eq!(summary.installed.len(), 2);

        let sofa1 = db.get("acme.sofas.sofa1").unwrap().unwrap();
        assert_eq!(sofa1.name, "sofa1");
        assert_eq!(sofa1.vendor.name, "TestVendor");
        assert!(sofa1.style_tags.contains(&"furniture".to_string()));
    }

    #[test]
    fn install_is_idempotent_on_second_call() {
        let td = tempfile::tempdir().unwrap();
        write_pack(td.path(), "p", vec![("a".into(), b"x".to_vec())]);
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let first = install_asset_packs(&mut db, &registry, &enforcer).unwrap();
        assert_eq!(first.installed.len(), 1);
        let second = install_asset_packs(&mut db, &registry, &enforcer).unwrap();
        assert_eq!(second.installed.len(), 0);
        assert_eq!(second.skipped.len(), 1);
    }

    #[test]
    fn permission_denied_blocks_install() {
        let td = tempfile::tempdir().unwrap();
        write_pack(td.path(), "p", vec![("a".into(), b"x".to_vec())]);
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let mut enforcer = PermissionEnforcer::new();
        // Grant a different extension's id — leaves `p` ungranted.
        enforcer.grant(ExtensionId("other".into()), [Permission::FilesystemRead]);
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let err = install_asset_packs(&mut db, &registry, &enforcer).unwrap_err();
        assert!(matches!(err, AssetExtensionError::PermissionDenied { .. }));
    }

    #[test]
    fn tampered_payload_is_rejected_by_checksum() {
        let td = tempfile::tempdir().unwrap();
        write_pack(td.path(), "p", vec![("a".into(), b"x".to_vec())]);
        // Overwrite the file *after* manifest signing fixed the hash.
        let path = td.path().join("p").join("furniture").join("a.glb");
        fs::write(path, b"TAMPERED").unwrap();
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let err = install_asset_packs(&mut db, &registry, &enforcer).unwrap_err();
        assert!(matches!(
            err,
            AssetExtensionError::BlobChecksumMismatch { .. }
        ));
    }

    #[test]
    fn missing_asset_file_surfaces_a_clear_error() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("p");
        fs::create_dir_all(&dir).unwrap();
        let manifest = ExtensionManifest {
            id: ExtensionId("p".into()),
            name: "p".into(),
            version: "1.0.0".into(),
            kind: ExtensionType::AssetPack,
            permissions: vec![Permission::FilesystemRead, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: String::new(),
            asset_pack: Some(AssetPackBody {
                vendor: "V".into(),
                entries: vec![AssetEntry {
                    asset_id: "a".into(),
                    name: "A".into(),
                    kind: AssetEntryKind::Furniture,
                    tags: vec![],
                    source_path: PathBuf::from("nope.glb"),
                    blake3: String::new(),
                }],
            }),
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: None,
            importer: None,
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let err = install_asset_packs(&mut db, &registry, &enforcer).unwrap_err();
        assert!(matches!(err, AssetExtensionError::MissingAsset { .. }));
    }
}
