//! Strongly-typed identifiers and small enums used across AEC Studio.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AecError, AecResult};

macro_rules! typed_id {
    ($name:ident, $prefix:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

typed_id!(
    ProjectId,
    "proj",
    "Identifier for an AEC Studio project package."
);
typed_id!(
    EntityId,
    "ent",
    "Identifier for a single entity in the project graph."
);

impl EntityId {
    /// Derive a deterministic [`EntityId`] from an external identity
    /// seed (e.g. an `IfcGloballyUniqueId` recovered from an external
    /// BIM authoring tool's export). The same seed always produces the
    /// same `EntityId` across runs and across processes.
    ///
    /// ## Why
    ///
    /// Default-constructed `EntityId`s use a UUIDv4 — non-deterministic
    /// per call. That's the right behaviour for entities authored
    /// inside AEC Studio: an author placing two walls produces two
    /// distinct ids. But for entities recovered from an external file
    /// that doesn't carry an `EntityId` literal (e.g. a Revit-authored
    /// `IfcBuildingStorey`), we need the parser to produce the
    /// **same** id every time it parses the same row, so the bridge's
    /// `bim_attach_ifc` dedup index can correctly identify "this
    /// spatial node already exists in the project database" by id
    /// alone, without an extra `guid → id` side table.
    ///
    /// Backed by UUIDv5 with a fixed AEC Studio namespace UUID, so:
    ///
    /// * It's a true deterministic function of the seed (RFC 4122 §4.3),
    ///   collision resistance is bounded by the 122-bit UUID space.
    /// * Two different namespaces (e.g. a future
    ///   `Self::from_dwg_handle`) won't collide with IFC-derived ids
    ///   even if their seeds happen to overlap.
    pub fn from_guid_seed(seed: &str) -> Self {
        // Fixed AEC Studio namespace UUID for IFC-derived entities.
        // Derivation: the first 4 bytes spell "aec5" (≈ "AEC Studio")
        // followed by 12 random bytes generated once with `uuidgen`.
        // The literal is pinned here forever; if this constant changes,
        // all previously-attached BIM snapshots will lose dedup
        // continuity (they'd be reported as `inserted` on the next
        // re-attach, with the old rows orphaned), so the test
        // `entity_id_from_guid_seed_namespace_pin` asserts the exact
        // UUIDv5 output for a known seed to catch accidental drift.
        // Treat as load-bearing.
        const NAMESPACE: Uuid = Uuid::from_u128(0xaec5_70d1_0fc4_4ec8_a2ed_cb44_9d4f_1e55_u128);
        let uuid = Uuid::new_v5(&NAMESPACE, seed.as_bytes());
        Self(format!("ent_{}", uuid.simple()))
    }
}

typed_id!(
    CommandId,
    "cmd",
    "Identifier for a single executed command."
);
typed_id!(
    DiffId,
    "diff",
    "Identifier for a previewable diff produced by a command or AI plan."
);

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
        &[
            Self::Design,
            Self::Draft,
            Self::Bim,
            Self::Render,
            Self::Deliver,
        ]
    }

    /// Parse a [`Scope`] from its [`Self::as_str`] representation
    /// (`"design"` / `"draft"` / `"bim"` / `"render"` / `"deliver"`).
    /// Returns [`AecError::Other`] for any other value so the round-trip
    /// `as_str` → `parse` is total over the enum and rejects any
    /// stray-or-stale value at the seam (SQL → struct, JSON → struct).
    pub fn parse(s: &str) -> AecResult<Self> {
        match s {
            "design" => Ok(Self::Design),
            "draft" => Ok(Self::Draft),
            "bim" => Ok(Self::Bim),
            "render" => Ok(Self::Render),
            "deliver" => Ok(Self::Deliver),
            other => Err(AecError::Other(format!(
                "unknown scope `{other}` (expected one of design / draft / bim / render / deliver)"
            ))),
        }
    }
}

impl FromStr for Scope {
    type Err = AecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an action originated from a user gesture, an AI tool call,
/// or an external integration like KChat.
///
/// `KChat` is used when a KChat review comment is ingested into the
/// audit trail (see `aec_core::kchat::ingest_review`). KChat entries
/// are always *read-only* with respect to project data — the variant
/// exists only so the audit chain can attribute the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    User,
    Ai,
    KChat,
}

/// An actor record attached to every command and audit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    /// For `ActorKind::Ai`, the name of the tool that produced the action
    /// (e.g. `style_assistant`, `plan_detection`). For
    /// `ActorKind::KChat`, the commenter handle (e.g. `@alice`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl Actor {
    pub fn user() -> Self {
        Self {
            kind: ActorKind::User,
            tool: None,
        }
    }

    pub fn ai(tool: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Ai,
            tool: Some(tool.into()),
        }
    }

    /// Convenience predicate — the audit-trail UI, the AI panel's
    /// "by" badge, and `ai_apply`'s unit tests all want a one-line
    /// check for AI-attributed actors. Adding it here keeps the
    /// `ActorKind` enum private to the type and prevents callers
    /// from importing `ActorKind` just to discriminate on it.
    pub fn is_ai(&self) -> bool {
        self.kind == ActorKind::Ai
    }

    /// Build a KChat-sourced actor. The `commenter` handle is stored
    /// in the `tool` field so the audit-trail viewer can show who
    /// posted the review comment without a separate column.
    pub fn kchat(commenter: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::KChat,
            tool: Some(commenter.into()),
        }
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
///
/// Serializes as lowercase (`"eu"`, `"na"`, `"apac"`). Template JSON files
/// authored by humans tend to use uppercase codes (`"EU"`, `"NA"`, `"APAC"`),
/// so the deserializer also accepts those via `#[serde(alias = ...)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// Europe (ISO sheet sizes, IFC4 default, millimeters).
    #[serde(alias = "EU")]
    Eu,
    /// North America (ANSI sheet sizes, IFC4 default, inches/feet).
    #[serde(alias = "NA")]
    Na,
    /// Asia-Pacific (ISO sheet sizes, millimeters).
    #[serde(alias = "APAC")]
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
    fn entity_id_from_guid_seed_is_deterministic() {
        let seed = "00000000000000000000a6";
        let a = EntityId::from_guid_seed(seed);
        let b = EntityId::from_guid_seed(seed);
        assert_eq!(a, b, "same seed must produce the same EntityId");
        assert!(a.as_str().starts_with("ent_"));
        assert_eq!(
            a.as_str().len(),
            "ent_".len() + 32,
            "UUIDv5 simple form is 32 hex chars"
        );
    }

    #[test]
    fn entity_id_from_guid_seed_diverges_on_different_seeds() {
        let a = EntityId::from_guid_seed("00000000000000000000a6");
        let b = EntityId::from_guid_seed("00000000000000000000a7");
        assert_ne!(a, b);
    }

    #[test]
    fn entity_id_from_guid_seed_namespace_pin() {
        // Pin the namespace UUID. If this test fails it means the
        // namespace constant in `EntityId::from_guid_seed` changed,
        // which would orphan every previously-attached BIM
        // snapshot. Treat any change here as a backwards-incompatible
        // migration; the namespace is load-bearing for re-attach
        // dedup.
        assert_eq!(
            EntityId::from_guid_seed("AEC-Studio fixture pin").as_str(),
            "ent_7759ca01f97959cb9592d55c929f018c",
        );
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

    #[test]
    fn region_deserializes_uppercase_aliases() {
        // Shipped template JSON files use uppercase region codes.
        assert_eq!(
            serde_json::from_str::<Region>("\"EU\"").unwrap(),
            Region::Eu
        );
        assert_eq!(
            serde_json::from_str::<Region>("\"NA\"").unwrap(),
            Region::Na
        );
        assert_eq!(
            serde_json::from_str::<Region>("\"APAC\"").unwrap(),
            Region::Apac
        );
    }
}
