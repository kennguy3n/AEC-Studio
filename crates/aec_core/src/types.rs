//! Strongly-typed identifiers and small enums used across AEC Studio.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AecError, AecResult};

macro_rules! typed_id {
    ($name:ident, $prefix:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a fresh, prefix-tagged id backed by a UUIDv4.
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Uuid::new_v4().simple()))
            }

            /// Build an id from an existing string, validating the prefix.
            pub fn from_string(s: impl Into<String>) -> AecResult<Self> {
                let s = s.into();
                if !s.starts_with(concat!($prefix, "_")) {
                    return Err(AecError::InvalidId {
                        kind: stringify!($name),
                        value: s,
                    });
                }
                Ok(Self(s))
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = AecError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::from_string(s)
            }
        }
    };
}

typed_id!(ProjectId, "proj", "Identifier for an AEC Studio project package.");
typed_id!(EntityId, "ent", "Identifier for a single entity in the project graph.");
typed_id!(CommandId, "cmd", "Identifier for a single executed command.");
typed_id!(DiffId, "diff", "Identifier for a previewable diff produced by a command or AI plan.");

/// The five user-facing workflow modes plus the `Home` dashboard scope.
///
/// `Scope` is used to (a) route commands to the right tools and inspectors,
/// (b) constrain AI tool calls to the active workflow surface, and (c) tag
/// audit-log entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Design,
    Draft,
    Bim,
    Render,
    Deliver,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Draft => "draft",
            Self::Bim => "bim",
            Self::Render => "render",
            Self::Deliver => "deliver",
        }
    }

    /// Iterate all scopes in canonical mode-rail order.
    pub fn all() -> &'static [Self] {
        &[Self::Design, Self::Draft, Self::Bim, Self::Render, Self::Deliver]
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an action originated from a user gesture or an AI tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    User,
    Ai,
}

/// An actor record attached to every command and audit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    /// For `ActorKind::Ai`, the name of the tool that produced the action
    /// (e.g. `style_assistant`, `plan_detection`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl Actor {
    pub fn user() -> Self {
        Self { kind: ActorKind::User, tool: None }
    }

    pub fn ai(tool: impl Into<String>) -> Self {
        Self { kind: ActorKind::Ai, tool: Some(tool.into()) }
    }
}

/// Length unit families supported by AEC Studio.
///
/// The active unit is a project-level setting (`Settings.units`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Units {
    /// Millimeters (default for EU/APAC interior workflows).
    Mm,
    /// Meters (default for architectural workflows).
    M,
    /// Inches (default for US interior workflows).
    Inches,
    /// Feet (default for US architectural workflows).
    Feet,
}

impl Units {
    /// Conversion factor to millimeters (the canonical internal unit).
    pub fn to_mm(self, value: f64) -> f64 {
        match self {
            Self::Mm => value,
            Self::M => value * 1000.0,
            Self::Inches => value * 25.4,
            Self::Feet => value * 304.8,
        }
    }

    /// Convert from millimeters back to this unit.
    pub fn from_mm(self, mm: f64) -> f64 {
        match self {
            Self::Mm => mm,
            Self::M => mm / 1000.0,
            Self::Inches => mm / 25.4,
            Self::Feet => mm / 304.8,
        }
    }
}

/// Regional defaults bundle. Controls default units, sheet sizes, ANSI vs ISO
/// drawing conventions, and BIM classification preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// Europe (ISO sheet sizes, IFC4 default, millimeters).
    Eu,
    /// North America (ANSI sheet sizes, IFC4 default, inches/feet).
    Na,
    /// Asia-Pacific (ISO sheet sizes, millimeters).
    Apac,
}

impl Region {
    pub fn default_units(self) -> Units {
        match self {
            Self::Eu | Self::Apac => Units::Mm,
            Self::Na => Units::Inches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_roundtrip_through_json() {
        let pid = ProjectId::new();
        let json = serde_json::to_string(&pid).expect("serialize");
        let pid2: ProjectId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(pid, pid2);
        assert!(pid.as_str().starts_with("proj_"));
    }

    #[test]
    fn typed_ids_reject_wrong_prefix() {
        let err = ProjectId::from_string("ent_abc").unwrap_err();
        match err {
            AecError::InvalidId { kind, .. } => assert_eq!(kind, "ProjectId"),
            other => panic!("expected InvalidId, got {other:?}"),
        }
    }

    #[test]
    fn typed_ids_parse_with_fromstr() {
        let raw = "ent_aabbccdd";
        let id: EntityId = raw.parse().expect("parse");
        assert_eq!(id.as_str(), raw);
    }

    #[test]
    fn scope_serializes_to_lowercase() {
        let s = serde_json::to_string(&Scope::Design).unwrap();
        assert_eq!(s, "\"design\"");
    }

    #[test]
    fn scope_all_covers_five_modes() {
        let modes: Vec<&str> = Scope::all().iter().map(|s| s.as_str()).collect();
        assert_eq!(modes, vec!["design", "draft", "bim", "render", "deliver"]);
    }

    #[test]
    fn actor_user_has_no_tool() {
        let actor = Actor::user();
        let s = serde_json::to_string(&actor).unwrap();
        assert!(!s.contains("tool"));
    }

    #[test]
    fn actor_ai_serializes_tool() {
        let actor = Actor::ai("style_assistant");
        let value: serde_json::Value = serde_json::to_value(&actor).unwrap();
        assert_eq!(value["kind"], "ai");
        assert_eq!(value["tool"], "style_assistant");
    }

    #[test]
    fn units_roundtrip_through_mm() {
        for unit in [Units::Mm, Units::M, Units::Inches, Units::Feet] {
            let original = 12.345_f64;
            let mm = unit.to_mm(original);
            let back = unit.from_mm(mm);
            assert!((back - original).abs() < 1e-9);
        }
    }

    #[test]
    fn region_defaults_map_to_units() {
        assert!(matches!(Region::Eu.default_units(), Units::Mm));
        assert!(matches!(Region::Apac.default_units(), Units::Mm));
        assert!(matches!(Region::Na.default_units(), Units::Inches));
    }

    #[test]
    fn region_roundtrip_through_json() {
        for region in [Region::Eu, Region::Na, Region::Apac] {
            let s = serde_json::to_string(&region).unwrap();
            let back: Region = serde_json::from_str(&s).unwrap();
            assert_eq!(region, back);
        }
    }
}
