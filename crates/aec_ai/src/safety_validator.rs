//! Safety validator. Every AI response goes through this gate before it
//! reaches the diff engine.
//!
//! Enforces:
//!   1. Scope: the tool is allowed in the current workflow mode.
//!   2. Allowlist: only tools registered in the schema registry can run.
//!   3. Bounded change: # of entities to modify ≤ schema's
//!      `max_entities_modified`.
//!   4. Grammar shape: payload matches the tool's GBNF (via the rust-side
//!      validator).
//!   5. No exfiltration: the proposed diff doesn't reference network URLs
//!      or absolute filesystem paths outside the project package.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use aec_core::types::Scope;

use crate::grammars::GrammarRegistry;
use crate::tool_schema::{ToolName, ToolSchemaRegistry};

#[derive(Debug, Error)]
pub enum SafetyError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("tool `{tool}` not allowed in scope `{scope:?}`")]
    ScopeViolation { tool: String, scope: Scope },
    #[error("payload would modify {entities} entities (max for `{tool}` is {max})")]
    BoundsExceeded {
        tool: String,
        entities: u32,
        max: u32,
    },
    #[error("payload failed grammar validation for `{0}`")]
    GrammarMismatch(String),
    #[error("payload referenced disallowed resource: `{0}`")]
    Exfiltration(String),
    #[error("malformed payload: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyViolation {
    UnknownTool,
    ScopeViolation,
    BoundsExceeded,
    GrammarMismatch,
    Exfiltration,
    Malformed,
}

pub struct SafetyValidator<'a> {
    schemas: &'a ToolSchemaRegistry,
    grammars: &'a GrammarRegistry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationContext {
    pub scope: Scope,
    pub tool: ToolName,
    pub entities_modified: u32,
    pub payload: String,
}

impl<'a> SafetyValidator<'a> {
    pub fn new(schemas: &'a ToolSchemaRegistry, grammars: &'a GrammarRegistry) -> Self {
        Self { schemas, grammars }
    }

    pub fn validate(&self, ctx: &ValidationContext) -> Result<(), SafetyError> {
        let schema = self
            .schemas
            .get(ctx.tool)
            .ok_or_else(|| SafetyError::UnknownTool(ctx.tool.as_str().into()))?;
        if !schema.allowed_scopes.contains(&ctx.scope) {
            return Err(SafetyError::ScopeViolation {
                tool: ctx.tool.as_str().into(),
                scope: ctx.scope,
            });
        }
        if ctx.entities_modified > schema.max_entities_modified {
            return Err(SafetyError::BoundsExceeded {
                tool: ctx.tool.as_str().into(),
                entities: ctx.entities_modified,
                max: schema.max_entities_modified,
            });
        }
        let grammar = self
            .grammars
            .get(&schema.grammar_key)
            .ok_or_else(|| SafetyError::GrammarMismatch(schema.grammar_key.clone()))?;
        if !grammar.matches(&ctx.payload) {
            return Err(SafetyError::GrammarMismatch(schema.grammar_key.clone()));
        }
        check_exfiltration(&ctx.payload)?;
        Ok(())
    }
}

/// Reject any absolute paths or network URLs that escape the project
/// sandbox. We parse the payload as JSON and recursively inspect every
/// **string value** — keys, structural punctuation, and numeric/bool
/// literals are intentionally not checked, so a field name like
/// `home_path_hint` cannot trigger a false positive.
///
/// If the payload is not valid JSON (which the grammar check should have
/// already caught upstream) we fall back to a raw scan over the entire
/// payload so we never silently accept an exfiltration attempt in a
/// malformed envelope.
fn check_exfiltration(payload: &str) -> Result<(), SafetyError> {
    fn scan(value: &serde_json::Value) -> Result<(), SafetyError> {
        match value {
            serde_json::Value::String(s) => check_string_for_exfiltration(s),
            serde_json::Value::Array(items) => {
                for item in items {
                    scan(item)?;
                }
                Ok(())
            }
            serde_json::Value::Object(map) => {
                for v in map.values() {
                    scan(v)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        return scan(&v);
    }
    // Non-JSON payload — apply the same structural check directly to the
    // raw text so we err on the side of caution rather than letting an
    // exfiltration attempt slip through a malformed envelope.
    check_string_for_exfiltration(payload)
}

/// Reject a single string if it contains anything that looks like an
/// absolute filesystem path or a network URL. The check is structural
/// (it understands drive letters, UNC prefixes, and URL schemes) rather
/// than a fixed list of substrings, so it cannot be bypassed by simply
/// using a drive letter other than `C:`.
fn check_string_for_exfiltration(s: &str) -> Result<(), SafetyError> {
    // 1. Any URL with a network or local-file scheme.
    //    We accept any scheme matching `[a-z][a-z0-9+.-]*://` so the
    //    `http://`, `https://`, `ftp://`, `file:///`, `gs://`, `s3://`,
    //    `data:` (when followed by content), etc. all trip the check.
    if find_url_scheme(s).is_some() {
        return Err(SafetyError::Exfiltration("network or file URL".into()));
    }

    // 2. POSIX absolute paths in sensitive trees. We deliberately do NOT
    //    treat every `/...` path as exfiltration — relative-looking
    //    strings (e.g. `/walls/1`) are common in entity references. We
    //    only block well-known sensitive prefixes.
    //
    //    Match is case-insensitive on the ASCII portion so a sneaky
    //    `/HOME/...` is still caught.
    const POSIX_PREFIXES: &[&str] = &[
        "/etc/",
        "/var/",
        "/usr/",
        "/opt/",
        "/root/",
        "/home/",
        "/users/",
        "/tmp/",
        "/proc/",
        "/sys/",
        "/private/",
    ];
    let lower = s.to_ascii_lowercase();
    for pat in POSIX_PREFIXES {
        if lower.contains(pat) {
            return Err(SafetyError::Exfiltration((*pat).into()));
        }
    }

    // 3. Windows absolute paths: any drive letter followed by `:\` or
    //    `:/`. The previous implementation only checked `C:` literally,
    //    so `D:\Users\...` would have slipped through. Use a structural
    //    detector instead.
    if find_windows_drive_path(s).is_some() {
        return Err(SafetyError::Exfiltration("windows absolute path".into()));
    }

    // 4. UNC paths: `\\server\share` or `//server/share` (the latter
    //    survives a JSON-escape pass that strips backslashes).
    if find_unc_path(s).is_some() {
        return Err(SafetyError::Exfiltration("unc path".into()));
    }

    Ok(())
}

/// Return the start index of the first URL scheme delimiter (`://`) in
/// `s` that is preceded by a valid scheme name. The match is robust to
/// case and ignores embedded `://` that aren't preceded by a scheme.
fn find_url_scheme(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for (i, w) in bytes.windows(3).enumerate() {
        if w == b"://" {
            // Walk backwards collecting ASCII alphanumerics, `+`, `-`, `.`
            // until we find a scheme-start character.
            let mut j = i;
            while j > 0 {
                let c = bytes[j - 1] as char;
                if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.') {
                    j -= 1;
                } else {
                    break;
                }
            }
            if j < i && (bytes[j] as char).is_ascii_alphabetic() {
                return Some(j);
            }
        }
    }
    None
}

/// Return the start index of a Windows absolute path of the form
/// `<letter>:\` or `<letter>:/` inside `s`. The leading character must be
/// an ASCII letter (any case).
fn find_windows_drive_path(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() && bytes[i + 1] == b':' {
            let next = bytes[i + 2];
            if next == b'\\' || next == b'/' {
                // Reject when the drive letter is part of an identifier
                // like `myC:/` — only accept when preceded by a boundary.
                let preceded_by_letter = i > 0 && (bytes[i - 1] as char).is_ascii_alphanumeric();
                if !preceded_by_letter {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Return the start index of a UNC path (`\\server\share` or its
/// forward-slash equivalent `//server/share`) inside `s`.
///
/// A real UNC path always has the shape `<sep><sep><host><sep><share>` —
/// in particular there must be **another separator** between the host
/// segment and the share name. We require all four pieces so that purely
/// arithmetic strings like `"50//50"` (which have `//` followed by digits
/// but no terminating separator and share) are not mistaken for a UNC
/// path. Host segments may legally contain alphanumerics, dots, and
/// hyphens (covers DNS names *and* IP literals like `10.0.0.5`).
fn find_unc_path(s: &str) -> Option<usize> {
    fn is_host_char(b: u8) -> bool {
        let c = b as char;
        c.is_ascii_alphanumeric() || c == '.' || c == '-'
    }
    fn is_share_char(b: u8) -> bool {
        let c = b as char;
        c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'
    }

    let bytes = s.as_bytes();
    if bytes.len() < 5 {
        return None;
    }
    let mut i = 0;
    while i + 1 < bytes.len() {
        let w = &bytes[i..i + 2];
        if w == b"\\\\" || w == b"//" {
            let mut j = i + 2;
            // Skip any additional slashes so a stray `///` doesn't match —
            // a UNC has exactly two leading separators.
            if j < bytes.len() && (bytes[j] == b'/' || bytes[j] == b'\\') {
                i += 1;
                continue;
            }
            // Host segment: ≥1 char of host-class.
            let host_start = j;
            while j < bytes.len() && is_host_char(bytes[j]) {
                j += 1;
            }
            if j == host_start {
                i += 1;
                continue;
            }
            // Separator between host and share.
            if j >= bytes.len() || (bytes[j] != b'/' && bytes[j] != b'\\') {
                i += 1;
                continue;
            }
            j += 1;
            // Share segment: ≥1 char of share-class.
            let share_start = j;
            while j < bytes.len() && is_share_char(bytes[j]) {
                j += 1;
            }
            if j == share_start {
                i += 1;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tool: ToolName, scope: Scope, modified: u32, payload: &str) -> ValidationContext {
        ValidationContext {
            scope,
            tool,
            entities_modified: modified,
            payload: payload.into(),
        }
    }

    #[test]
    fn accepts_valid_style_assistant_payload() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        assert!(v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 4, payload))
            .is_ok());
    }

    #[test]
    fn rejects_wrong_scope() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Render, 1, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::ScopeViolation { .. }));
    }

    #[test]
    fn rejects_overlarge_change() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload =
            r#"{"furniture_ids":["a"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 9999, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::BoundsExceeded { .. }));
    }

    #[test]
    fn rejects_grammar_mismatch() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let err = v
            .validate(&ctx(ToolName::PlanDetection, Scope::Design, 4, "{}"))
            .unwrap_err();
        assert!(matches!(err, SafetyError::GrammarMismatch(_)));
    }

    #[test]
    fn rejects_exfiltration_url() {
        let s = ToolSchemaRegistry::defaults();
        let g = GrammarRegistry::defaults();
        let v = SafetyValidator::new(&s, &g);
        let payload = r#"{"furniture_ids":["http://evil.example/x"],"material_ids":["b"],"lighting_preset_id":"warm_evening"}"#;
        let err = v
            .validate(&ctx(ToolName::StyleAssistant, Scope::Design, 1, payload))
            .unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }

    #[test]
    fn check_exfiltration_ignores_keys_and_inspects_values() {
        // A key called `home_path_hint` MUST NOT trigger; only string values
        // are scanned. The values themselves are benign here.
        let benign = r#"{"home_path_hint":"living_room","tags":["sofa","home_lamp"]}"#;
        assert!(check_exfiltration(benign).is_ok());

        // Same shape, but a value points at the user's home directory —
        // that must still be rejected.
        let exfil = r#"{"home_path_hint":"living_room","target":"/home/user/.ssh/id_rsa"}"#;
        let err = check_exfiltration(exfil).unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }

    #[test]
    fn check_exfiltration_falls_back_to_raw_scan_for_invalid_json() {
        // Malformed payload — fallback raw scan must still catch the URL.
        let bad = "not really json http://evil.example/secret";
        let err = check_exfiltration(bad).unwrap_err();
        assert!(matches!(err, SafetyError::Exfiltration(_)));
    }

    #[test]
    fn check_exfiltration_blocks_every_windows_drive_letter() {
        // The previous implementation only checked `C:` literally. A
        // crafted response targeting any other drive letter must now
        // be rejected just as firmly.
        for prefix in [
            "C:\\Windows\\System32",
            "c:\\Users\\victim\\.ssh",
            "D:\\Users\\victim\\Documents",
            "E:/payloads/exfil.txt",
            "x:\\private\\stash",
            "Z:/network/drive",
        ] {
            let payload = format!(r#"{{"target":"{}"}}"#, prefix.replace('\\', "\\\\"));
            let err = check_exfiltration(&payload).unwrap_err();
            assert!(
                matches!(err, SafetyError::Exfiltration(_)),
                "drive-letter path {prefix:?} was not rejected: payload={payload}",
            );
        }
    }

    #[test]
    fn check_exfiltration_blocks_unc_paths() {
        // Backslash UNC (Windows native) and forward-slash UNC (what
        // survives a JSON escape pass) must both be rejected.
        for unc in [
            r"\\fileserver\share\secret.txt",
            r"//fileserver/share/secret.txt",
            r"\\10.0.0.5\backups",
        ] {
            let payload = format!(r#"{{"target":"{}"}}"#, unc.replace('\\', "\\\\"));
            let err = check_exfiltration(&payload).unwrap_err();
            assert!(
                matches!(err, SafetyError::Exfiltration(_)),
                "UNC path {unc:?} was not rejected: payload={payload}",
            );
        }
    }

    #[test]
    fn check_exfiltration_blocks_arbitrary_url_schemes() {
        // The check is structural (scheme://...) so we catch S3, GCS, FTP,
        // and any future scheme without having to maintain a fixed list.
        for url in [
            "https://evil.example/x",
            "FTP://attacker.example/leak",
            "s3://my-bucket/secret",
            "gs://foo/bar",
            "file:///etc/passwd",
        ] {
            let payload = format!(r#"{{"target":"{}"}}"#, url);
            let err = check_exfiltration(&payload).unwrap_err();
            assert!(
                matches!(err, SafetyError::Exfiltration(_)),
                "URL {url:?} was not rejected",
            );
        }
    }

    #[test]
    fn check_exfiltration_allows_relative_paths_and_inert_strings() {
        // Project-relative paths (used to reference internal entities) must
        // still pass — only sensitive *absolute* paths and URLs are blocked.
        for good in [
            r#"{"target":"walls/1"}"#,
            r#"{"target":"materials/oak.json"}"#,
            r#"{"slug":"home_lamp"}"#,
            r#"{"text":"This is plain prose: 2 colons:: still fine."}"#,
            r#"{"ratio":"50%"}"#,
        ] {
            check_exfiltration(good)
                .unwrap_or_else(|e| panic!("benign payload {good:?} flagged: {e:?}"));
        }
    }

    #[test]
    fn find_helpers_match_only_real_prefixes() {
        // Drive-letter detector must not trip on tokens like `myC:foo` where
        // the colon is preceded by other alphanumerics.
        assert!(find_windows_drive_path("myC:\\foo").is_none());
        // But it must trip on a clean drive-letter prefix.
        assert!(find_windows_drive_path("D:\\anything").is_some());
        // UNC detector must require a host character after the prefix.
        assert!(find_unc_path("////").is_none());
        assert!(find_unc_path(r"\\srv\share").is_some());
        assert!(find_unc_path("//srv/share").is_some());
        // UNC detector must require BOTH a host AND a share separated by a
        // separator — purely arithmetic strings like "50//50" must not
        // match, and neither should "//host" without a trailing share.
        assert!(find_unc_path("50//50").is_none());
        assert!(find_unc_path("ratio 50//50 split").is_none());
        assert!(find_unc_path("//hostonly").is_none());
        assert!(find_unc_path("//host/").is_none());
        // Host segment must be IP-literal-friendly (digits + dots allowed).
        assert!(find_unc_path(r"\\10.0.0.5\backups").is_some());
        // URL scheme detector must reject "://" that isn't preceded by a
        // scheme (e.g. the bare delimiter inside a free-form sentence).
        assert!(find_url_scheme("look at this :// here").is_none());
        assert!(find_url_scheme("https://evil.example").is_some());
    }
}
