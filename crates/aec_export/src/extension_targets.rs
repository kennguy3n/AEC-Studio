//! Host integration for `ExtensionType::ExportTarget` extensions.
//!
//! Reads the [`aec_core::ExportTargetBody`] of every export-target
//! extension and surfaces a stable [`ExtensionExportTarget`] descriptor
//! that the Deliver UI / IPC bridge can list alongside built-in targets
//! (PDF, XLSX, ZIP, IFC).
//!
//! Permission gate: every export target requires
//! [`aec_core::Permission::FilesystemWrite`] because the act of running
//! the target writes a deliverable to disk.

use std::path::PathBuf;

use thiserror::Error;

use aec_core::{
    ExportFormat as ExtExportFormat, ExtensionRegistry, ExtensionType, LoadedExtension, Operation,
    PermissionCheck, PermissionEnforcer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Pdf,
    Xlsx,
    Zip,
    Ifc,
    Json,
    Glb,
}

impl From<ExtExportFormat> for ExportFormat {
    fn from(value: ExtExportFormat) -> Self {
        match value {
            ExtExportFormat::Pdf => ExportFormat::Pdf,
            ExtExportFormat::Xlsx => ExportFormat::Xlsx,
            ExtExportFormat::Zip => ExportFormat::Zip,
            ExtExportFormat::Ifc => ExportFormat::Ifc,
            ExtExportFormat::Json => ExportFormat::Json,
            ExtExportFormat::Glb => ExportFormat::Glb,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionExportTarget {
    /// Stable id from the extension manifest (`target_id`).
    pub target_id: String,
    /// Human-readable name surfaced in the Deliver UI.
    pub display_name: String,
    pub format: ExportFormat,
    /// Default file extension (no leading dot) for output suggestions.
    pub default_extension: String,
    /// Absolute path to the extension's entry point script / executable.
    /// Hosts that don't yet support running custom code can still surface
    /// the target as "unavailable on this build" without erroring out.
    pub entry_path: PathBuf,
    /// Originating extension id, used for audit logging.
    pub extension_id: String,
    /// Version from the extension manifest.
    pub extension_version: String,
}

#[derive(Debug, Error, PartialEq)]
pub enum ExportTargetExtensionError {
    #[error("permission denied for extension {ext}: {reason}")]
    PermissionDenied { ext: String, reason: String },
    #[error("extension {ext} has no export_target body")]
    NotAnExportTarget { ext: String },
}

/// Resolve one extension into an [`ExtensionExportTarget`] descriptor.
pub fn resolve_export_target(
    ext: &LoadedExtension,
    enforcer: &PermissionEnforcer,
) -> Result<ExtensionExportTarget, ExportTargetExtensionError> {
    if let PermissionCheck::Denied { reason } = enforcer.check_permission(
        &ext.manifest.id,
        &Operation::WriteFile {
            scope: format!("extension:{}", ext.manifest.id),
        },
    ) {
        return Err(ExportTargetExtensionError::PermissionDenied {
            ext: ext.manifest.id.0.clone(),
            reason,
        });
    }
    let Some(body) = ext.manifest.export_target.as_ref() else {
        return Err(ExportTargetExtensionError::NotAnExportTarget {
            ext: ext.manifest.id.0.clone(),
        });
    };
    Ok(ExtensionExportTarget {
        target_id: body.target_id.clone(),
        display_name: body.display_name.clone(),
        format: body.format.into(),
        default_extension: body.default_extension.clone(),
        entry_path: ext.root.join(&body.entry_path),
        extension_id: ext.manifest.id.0.clone(),
        extension_version: ext.manifest.version.clone(),
    })
}

/// Resolve every export-target extension in `registry`. Failed
/// resolutions are returned alongside successes so the UI can show
/// "unavailable" rows with their reason.
pub fn list_extension_export_targets(
    registry: &ExtensionRegistry,
    enforcer: &PermissionEnforcer,
) -> (Vec<ExtensionExportTarget>, Vec<ExportTargetExtensionError>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for ext in registry.by_kind(ExtensionType::ExportTarget) {
        match resolve_export_target(ext, enforcer) {
            Ok(t) => ok.push(t),
            Err(e) => errs.push(e),
        }
    }
    (ok, errs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::{
        ExportFormat as Fmt, ExportTargetBody, ExtensionId, ExtensionLoader, ExtensionManifest,
        ExtensionType, LoadOptions, Permission,
    };
    use std::fs;
    use std::path::Path;

    fn write_target(root: &Path) {
        let dir = root.join("acme.pdfpack");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("entry.lua"), "-- entry").unwrap();
        let manifest = ExtensionManifest {
            id: ExtensionId("acme.pdfpack".into()),
            name: "PDF Pack".into(),
            version: "2.0.0".into(),
            kind: ExtensionType::ExportTarget,
            permissions: vec![Permission::FilesystemWrite, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: "branded PDF".into(),
            asset_pack: None,
            template: None,
            schedule: None,
            export_target: Some(ExportTargetBody {
                target_id: "acme.pdfpack".into(),
                display_name: "ACME Branded PDF".into(),
                format: Fmt::Pdf,
                default_extension: "pdf".into(),
                entry_path: PathBuf::from("entry.lua"),
            }),
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
    fn extension_export_target_resolves_with_correct_format_and_path() {
        let td = tempfile::tempdir().unwrap();
        write_target(td.path());
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let (targets, errors) = list_extension_export_targets(&registry, &enforcer);
        assert!(errors.is_empty());
        assert_eq!(targets.len(), 1);
        let t = &targets[0];
        assert_eq!(t.target_id, "acme.pdfpack");
        assert_eq!(t.display_name, "ACME Branded PDF");
        assert_eq!(t.format, ExportFormat::Pdf);
        assert_eq!(t.default_extension, "pdf");
        assert!(t.entry_path.ends_with("entry.lua"));
        assert_eq!(t.extension_id, "acme.pdfpack");
        assert_eq!(t.extension_version, "2.0.0");
    }

    #[test]
    fn permission_denied_when_filesystem_write_is_absent() {
        let td = tempfile::tempdir().unwrap();
        write_target(td.path());
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        // No grants — write is denied.
        let enforcer = PermissionEnforcer::new();
        let (targets, errors) = list_extension_export_targets(&registry, &enforcer);
        assert!(targets.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ExportTargetExtensionError::PermissionDenied { .. }
        ));
    }
}
