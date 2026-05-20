//! Host integration for `ExtensionType::Schedule` extensions.
//!
//! Reads the [`aec_core::ScheduleBody`] of every schedule extension in a
//! registry and surfaces a [`ScheduleSheet`] header (columns + a single
//! deterministic placeholder row built from the column defaults) so the
//! schedule UI can offer the new schedule alongside the built-in
//! room/door/window/material sheets.
//!
//! Permission gate: extensions that declare a schedule type must have
//! [`aec_core::Permission::GeometryRead`]. Without it the host treats
//! the schedule as unavailable — the extension can still list itself in
//! the registry but won't produce a sheet until permission is granted.

use thiserror::Error;

use aec_core::{
    ExtensionRegistry, ExtensionType, LoadedExtension, Operation, PermissionCheck,
    PermissionEnforcer, ScheduleValueType,
};

use super::{ScheduleColumn, ScheduleSheet};

#[derive(Debug, Error, PartialEq)]
pub enum ScheduleExtensionError {
    #[error("permission denied for extension {ext}: {reason}")]
    PermissionDenied { ext: String, reason: String },
    #[error("extension {ext} has no schedule body")]
    NotASchedule { ext: String },
}

/// Build a [`ScheduleSheet`] from a schedule extension. The sheet has
/// the extension's declared columns and one default-valued row so that
/// downstream code (XLSX writer, UI) can render the schedule before any
/// real project data is bound.
pub fn build_extension_schedule(
    ext: &LoadedExtension,
    enforcer: &PermissionEnforcer,
) -> Result<ScheduleSheet, ScheduleExtensionError> {
    if let PermissionCheck::Denied { reason } =
        enforcer.check_permission(&ext.manifest.id, &Operation::ReadGeometry)
    {
        return Err(ScheduleExtensionError::PermissionDenied {
            ext: ext.manifest.id.0.clone(),
            reason,
        });
    }
    let Some(body) = ext.manifest.schedule.as_ref() else {
        return Err(ScheduleExtensionError::NotASchedule {
            ext: ext.manifest.id.0.clone(),
        });
    };

    let columns: Vec<ScheduleColumn> = body
        .columns
        .iter()
        .map(|c| ScheduleColumn {
            key: c.key.clone(),
            display: c.header.clone(),
        })
        .collect();

    let mut sheet = ScheduleSheet::new(body.display_name.clone(), columns);
    let cells: Vec<String> = body
        .columns
        .iter()
        .map(|c| {
            c.default
                .clone()
                .unwrap_or_else(|| default_for(c.value_type).to_string())
        })
        .collect();
    sheet.push_row(cells);
    Ok(sheet)
}

fn default_for(t: ScheduleValueType) -> &'static str {
    match t {
        ScheduleValueType::String => "",
        ScheduleValueType::Integer => "0",
        ScheduleValueType::Float => "0.0",
        ScheduleValueType::Boolean => "false",
    }
}

/// Build sheets for every schedule extension in `registry`. Errors from
/// individual extensions are returned alongside the successful sheets so
/// callers can surface partial failures in the UI.
pub fn build_all_extension_schedules(
    registry: &ExtensionRegistry,
    enforcer: &PermissionEnforcer,
) -> (Vec<ScheduleSheet>, Vec<ScheduleExtensionError>) {
    let mut sheets = Vec::new();
    let mut errors = Vec::new();
    for ext in registry.by_kind(ExtensionType::Schedule) {
        match build_extension_schedule(ext, enforcer) {
            Ok(s) => sheets.push(s),
            Err(e) => errors.push(e),
        }
    }
    (sheets, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::{
        ExtensionId, ExtensionLoader, ExtensionManifest, ExtensionType, LoadOptions, Permission,
        ScheduleBody, ScheduleColumnDef, ScheduleValueType,
    };
    use std::fs;
    use std::path::Path;

    fn write_schedule(root: &Path) {
        let dir = root.join("acme.flooring");
        fs::create_dir_all(&dir).unwrap();
        let manifest = ExtensionManifest {
            id: ExtensionId("acme.flooring".into()),
            name: "Flooring Schedule".into(),
            version: "1.0.0".into(),
            kind: ExtensionType::Schedule,
            permissions: vec![Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: "flooring schedule".into(),
            asset_pack: None,
            template: None,
            schedule: Some(ScheduleBody {
                schedule_id: "acme.flooring".into(),
                display_name: "Flooring".into(),
                columns: vec![
                    ScheduleColumnDef {
                        key: "room".into(),
                        header: "Room".into(),
                        value_type: ScheduleValueType::String,
                        default: None,
                    },
                    ScheduleColumnDef {
                        key: "area".into(),
                        header: "Area".into(),
                        value_type: ScheduleValueType::Float,
                        default: None,
                    },
                    ScheduleColumnDef {
                        key: "material".into(),
                        header: "Material".into(),
                        value_type: ScheduleValueType::String,
                        default: Some("Oak".into()),
                    },
                ],
                formulas: vec![],
            }),
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
    fn extension_schedule_becomes_a_sheet() {
        let td = tempfile::tempdir().unwrap();
        write_schedule(td.path());
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let (sheets, errors) = build_all_extension_schedules(&registry, &enforcer);
        assert!(errors.is_empty());
        assert_eq!(sheets.len(), 1);
        let s = &sheets[0];
        assert_eq!(s.title, "Flooring");
        assert_eq!(s.columns.len(), 3);
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.rows[0].cells[0], "");
        assert_eq!(s.rows[0].cells[1], "0.0");
        assert_eq!(s.rows[0].cells[2], "Oak");
    }

    #[test]
    fn permission_denied_surfaces_error() {
        let td = tempfile::tempdir().unwrap();
        write_schedule(td.path());
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        // Empty enforcer — nothing is granted.
        let enforcer = PermissionEnforcer::new();
        let (sheets, errors) = build_all_extension_schedules(&registry, &enforcer);
        assert!(sheets.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            ScheduleExtensionError::PermissionDenied { .. }
        ));
    }
}
