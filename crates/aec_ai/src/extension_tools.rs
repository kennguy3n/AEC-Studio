//! Host integration for `ExtensionType::AiTool` extensions.
//!
//! Reads the [`aec_core::AiToolBody`] of every AI-tool extension in a
//! registry, validates that the extension declares
//! [`aec_core::Permission::AiTools`], and surfaces an
//! [`ExtensionAiToolSchema`] descriptor the planner can dispatch
//! against. The descriptor mirrors the fields the built-in
//! [`crate::tool_schema::ToolSchema`] carries (`allowed_scopes`,
//! `max_entities_modified`, `grammar_key`) so the safety validator can
//! treat extension tools and built-in tools uniformly.
//!
//! Permission gate: every extension AI tool must declare
//! [`aec_core::Permission::AiTools`]. Without it the host treats the
//! tool as unavailable — the registry can still report it, but
//! [`resolve_extension_ai_tool`] returns a permission-denied error so
//! the planner won't dispatch to it.

use thiserror::Error;

use aec_core::{
    types::Scope, ExtensionRegistry, ExtensionType, LoadedExtension, Operation, PermissionCheck,
    PermissionEnforcer,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionAiToolSchema {
    /// Stable id from the extension manifest (`tool_id`). The planner
    /// dispatches on this string; it does NOT participate in the
    /// built-in [`crate::tool_schema::ToolName`] enum, which is closed.
    pub tool_id: String,
    pub display_name: String,
    pub description: String,
    pub allowed_scopes: Vec<Scope>,
    pub max_entities_modified: u32,
    pub grammar_key: String,
    /// Originating extension, used for audit logging + the UI tool
    /// picker grouping.
    pub extension_id: String,
    pub extension_version: String,
}

#[derive(Debug, Error, PartialEq)]
pub enum ExtensionAiToolError {
    #[error("permission denied for extension {ext}: {reason}")]
    PermissionDenied { ext: String, reason: String },
    #[error("extension {ext} has no ai_tool body")]
    NotAnAiTool { ext: String },
    #[error("extension {ext} declares unknown scope {scope}")]
    UnknownScope { ext: String, scope: String },
}

/// Resolve a single AI-tool extension into a schema. Returns an error
/// if the extension is missing the AI-tools permission or names an
/// unknown scope.
pub fn resolve_extension_ai_tool(
    ext: &LoadedExtension,
    enforcer: &PermissionEnforcer,
) -> Result<ExtensionAiToolSchema, ExtensionAiToolError> {
    if let PermissionCheck::Denied { reason } =
        enforcer.check_permission(&ext.manifest.id, &Operation::UseAiTool)
    {
        return Err(ExtensionAiToolError::PermissionDenied {
            ext: ext.manifest.id.0.clone(),
            reason,
        });
    }
    let Some(body) = ext.manifest.ai_tool.as_ref() else {
        return Err(ExtensionAiToolError::NotAnAiTool {
            ext: ext.manifest.id.0.clone(),
        });
    };
    let mut scopes = Vec::with_capacity(body.allowed_scopes.len());
    for raw in &body.allowed_scopes {
        scopes.push(parse_scope(raw).ok_or_else(|| ExtensionAiToolError::UnknownScope {
            ext: ext.manifest.id.0.clone(),
            scope: raw.clone(),
        })?);
    }
    Ok(ExtensionAiToolSchema {
        tool_id: body.tool_id.clone(),
        display_name: body.display_name.clone(),
        description: body.description.clone(),
        allowed_scopes: scopes,
        max_entities_modified: body.max_entities_modified,
        grammar_key: body.grammar_key.clone(),
        extension_id: ext.manifest.id.0.clone(),
        extension_version: ext.manifest.version.clone(),
    })
}

/// Resolve every AI-tool extension. Failures are returned alongside
/// successes so the UI can surface "unavailable" rows with a reason.
pub fn list_extension_ai_tools(
    registry: &ExtensionRegistry,
    enforcer: &PermissionEnforcer,
) -> (Vec<ExtensionAiToolSchema>, Vec<ExtensionAiToolError>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for ext in registry.by_kind(ExtensionType::AiTool) {
        match resolve_extension_ai_tool(ext, enforcer) {
            Ok(s) => ok.push(s),
            Err(e) => errs.push(e),
        }
    }
    (ok, errs)
}

/// Enforce the extension's declared `max_entities_modified` against a
/// proposed change set. Returns `Ok(())` when the change is within the
/// bound and `Err` otherwise. This is the extension half of the
/// built-in [`crate::safety_validator`] check.
pub fn enforce_max_entities_modified(
    schema: &ExtensionAiToolSchema,
    proposed_count: u32,
) -> Result<(), SafetyViolation> {
    if proposed_count > schema.max_entities_modified {
        Err(SafetyViolation::TooManyEntities {
            tool_id: schema.tool_id.clone(),
            proposed: proposed_count,
            cap: schema.max_entities_modified,
        })
    } else {
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum SafetyViolation {
    #[error("extension tool {tool_id} attempted to modify {proposed} entities (cap = {cap})")]
    TooManyEntities {
        tool_id: String,
        proposed: u32,
        cap: u32,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use aec_core::{
        AiToolBody, ExtensionId, ExtensionLoader, ExtensionManifest, ExtensionType, LoadOptions,
        Permission,
    };
    use std::fs;
    use std::path::Path;

    fn write_tool(root: &Path, allowed: Vec<&str>, cap: u32) {
        let dir = root.join("acme.layouter");
        fs::create_dir_all(&dir).unwrap();
        let manifest = ExtensionManifest {
            id: ExtensionId("acme.layouter".into()),
            name: "Layouter".into(),
            version: "1.2.0".into(),
            kind: ExtensionType::AiTool,
            permissions: vec![Permission::AiTools, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: "AI layout helper".into(),
            asset_pack: None,
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: Some(AiToolBody {
                tool_id: "acme.layouter".into(),
                display_name: "Layout Helper".into(),
                description: "Propose furniture layout".into(),
                allowed_scopes: allowed.into_iter().map(String::from).collect(),
                max_entities_modified: cap,
                grammar_key: "layout_suggestion".into(),
            }),
            importer: None,
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn ai_tool_extension_resolves_with_parsed_scopes() {
        let td = tempfile::tempdir().unwrap();
        write_tool(td.path(), vec!["design", "render"], 32);
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let (tools, errors) = list_extension_ai_tools(&registry, &enforcer);
        assert!(errors.is_empty());
        assert_eq!(tools.len(), 1);
        let t = &tools[0];
        assert_eq!(t.tool_id, "acme.layouter");
        assert_eq!(t.allowed_scopes, vec![Scope::Design, Scope::Render]);
        assert_eq!(t.max_entities_modified, 32);
        assert_eq!(t.grammar_key, "layout_suggestion");
    }

    #[test]
    fn unknown_scope_is_rejected_at_load_time() {
        // The aec_core loader runs `validate_manifest`, which already
        // rejects unknown scopes — extension AI tools never reach this
        // host with a bogus scope. Reaching the host path means the
        // loader was bypassed (e.g. in tests). Confirm the loader
        // surface rejects it cleanly so users see the error at install
        // time rather than at first dispatch.
        let td = tempfile::tempdir().unwrap();
        write_tool(td.path(), vec!["design", "bogus"], 4);
        let err = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bogus"),
            "loader error should mention the bogus scope, got: {msg}"
        );
    }

    #[test]
    fn host_directly_rejects_synthetic_unknown_scope() {
        // Build an ExtensionAiToolSchema-bearing LoadedExtension by hand
        // (bypassing the loader) so we can exercise the host's own
        // UnknownScope guard for defense-in-depth.
        use aec_core::LoadedExtension;
        let td = tempfile::tempdir().unwrap();
        let manifest = ExtensionManifest {
            id: ExtensionId("acme.layouter".into()),
            name: "Layouter".into(),
            version: "1.0.0".into(),
            kind: ExtensionType::AiTool,
            permissions: vec![Permission::AiTools, Permission::GeometryRead],
            signature: None,
            license: "AGPL-3.0".into(),
            description: String::new(),
            asset_pack: None,
            template: None,
            schedule: None,
            export_target: None,
            ai_tool: Some(AiToolBody {
                tool_id: "acme.layouter".into(),
                display_name: "L".into(),
                description: String::new(),
                allowed_scopes: vec!["design".into(), "outer-space".into()],
                max_entities_modified: 1,
                grammar_key: "layout_suggestion".into(),
            }),
            importer: None,
        };
        let ext = LoadedExtension {
            manifest,
            root: td.path().to_path_buf(),
            signed: false,
        };
        let mut enforcer = PermissionEnforcer::new();
        enforcer.grant(
            ExtensionId("acme.layouter".into()),
            [Permission::AiTools, Permission::GeometryRead],
        );
        let err = resolve_extension_ai_tool(&ext, &enforcer).unwrap_err();
        assert!(matches!(
            err,
            ExtensionAiToolError::UnknownScope { ref scope, .. } if scope == "outer-space"
        ));
    }

    #[test]
    fn safety_cap_is_enforced() {
        let td = tempfile::tempdir().unwrap();
        write_tool(td.path(), vec!["design"], 8);
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        let enforcer = PermissionEnforcer::from_registry(&registry);
        let (tools, _) = list_extension_ai_tools(&registry, &enforcer);
        let schema = &tools[0];
        assert!(enforce_max_entities_modified(schema, 8).is_ok());
        let err = enforce_max_entities_modified(schema, 9).unwrap_err();
        assert!(matches!(
            err,
            SafetyViolation::TooManyEntities {
                proposed: 9,
                cap: 8,
                ..
            }
        ));
    }

    #[test]
    fn missing_ai_tools_permission_blocks_resolution() {
        let td = tempfile::tempdir().unwrap();
        write_tool(td.path(), vec!["design"], 4);
        let registry = ExtensionLoader::new(td.path())
            .load(&LoadOptions::allow_unsigned())
            .unwrap();
        // Grant geometry_read but not ai_tools.
        let mut enforcer = PermissionEnforcer::new();
        enforcer.grant(
            ExtensionId("acme.layouter".into()),
            [Permission::GeometryRead],
        );
        let (tools, errors) = list_extension_ai_tools(&registry, &enforcer);
        assert!(tools.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ExtensionAiToolError::PermissionDenied { .. }
        ));
    }
}
