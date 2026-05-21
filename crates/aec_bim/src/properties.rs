//! Property sets (Pset_*), quantity sets (Qto_*), and custom psets.
//!
//! Property values are typed; the BIM cache fingerprints them with BLAKE3 so
//! we can detect changes when re-importing an IFC.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::IfcClass;
use crate::ifc::reader::unescape_step_string;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Text(String),
    Real(f64),
    Integer(i64),
    Boolean(bool),
    Length(f64),
    Area(f64),
    Volume(f64),
    Ratio(f64),
    /// IfcLabel — short, controlled string (≤ 255 chars).
    Label(String),
    /// Opaque IFC measure type AEC Studio doesn't model natively
    /// (e.g. `IfcMassDensityMeasure`, `IfcFrequencyMeasure`,
    /// `IfcCountMeasure`). The reader stores the measure tag in the
    /// uppercase STEP form (verbatim from the file, e.g.
    /// `"IFCMASSDENSITYMEASURE"`) together with the raw STEP value
    /// literal so the writer can round-trip the property losslessly
    /// without having to enumerate every IFC measure type in this
    /// enum. Recovering the IFC4 canonical camelCase spelling (e.g.
    /// `"IfcMassDensityMeasure"`) from the uppercase STEP form would
    /// require a dictionary; the writer normalises with
    /// `to_ascii_uppercase` either way, so round-tripping is exact
    /// regardless of which case the caller stored.
    Other {
        /// IFC measure-type tag as it appears in the STEP file —
        /// uppercased including the `IFC` prefix (e.g.
        /// `"IFCMASSDENSITYMEASURE"`). Programmatic constructors may
        /// also use the canonical camelCase spelling (e.g.
        /// `"IfcMassDensityMeasure"`); the writer uppercases before
        /// emitting either way.
        measure: String,
        /// Raw STEP literal as parsed (e.g. `"2400.0"`, `"'kg/m3'"`,
        /// `".T."`). The writer emits this back verbatim inside the
        /// `IFCXXX(...)` wrapper.
        raw: String,
    },
}

impl PropertyValue {
    /// IFC measure type for an instance — used when serialising back to
    /// an IFC `IfcPropertySingleValue.NominalValue` wrapper.
    pub fn ifc_measure_type(&self) -> &str {
        match self {
            Self::Text(_) => "IfcText",
            Self::Real(_) => "IfcReal",
            Self::Integer(_) => "IfcInteger",
            Self::Boolean(_) => "IfcBoolean",
            Self::Length(_) => "IfcLengthMeasure",
            Self::Area(_) => "IfcAreaMeasure",
            Self::Volume(_) => "IfcVolumeMeasure",
            Self::Ratio(_) => "IfcPositiveRatioMeasure",
            Self::Label(_) => "IfcLabel",
            Self::Other { measure, .. } => measure.as_str(),
        }
    }

    /// Raw STEP literal for the inner value, as it should appear
    /// inside the `IFCXXX(...)` measure wrapper. Returns `None` for
    /// variants whose serialisation requires the writer's escape
    /// logic (those go through the writer's normal formatters);
    /// returns `Some(raw)` only for [`PropertyValue::Other`], where
    /// the reader preserved the original bytes for verbatim
    /// round-trip.
    pub fn other_raw_literal(&self) -> Option<&str> {
        match self {
            Self::Other { raw, .. } => Some(raw.as_str()),
            _ => None,
        }
    }

    pub fn as_real(&self) -> Option<f64> {
        match self {
            Self::Real(v) | Self::Length(v) | Self::Area(v) | Self::Volume(v) | Self::Ratio(v) => {
                Some(*v)
            }
            Self::Integer(v) => Some(*v as f64),
            // For an opaque IFC measure the reader didn't model
            // natively (e.g. `IfcMassDensityMeasure(2400.0)`,
            // `IfcFrequencyMeasure(50.0)`, `IfcCountMeasure(12)`),
            // try to recover a numeric value from the raw STEP
            // literal so BOQ / schedules can still see it instead of
            // silently dropping the property. We deliberately do NOT
            // recurse through STEP escape parsing here — the only
            // case where this returns `Some` is when the raw literal
            // is a plain numeric token (the other lexical shapes for
            // an IFC measure value are quoted strings `'...'` or
            // booleans `.T.`/`.F.`, neither of which is meaningful
            // as a real). [`crate::ifc::reader::parse_step_real`]
            // handles ints (`12`), signed floats (`-3.5`), scientific
            // notation (`1.5e-3`), and IFC's legacy `D` / `d` exponent
            // variant (`1.5D-3` / `1.5d-3`) used by some pre-2010
            // FORTRAN-derived IFC exporters (canonical ISO 10303-21
            // only allows `e`/`E`, but the wider IFC ecosystem still
            // ships archives that use `D`).
            Self::Other { raw, .. } => crate::ifc::reader::parse_step_real(raw),
            // Non-numeric typed variants: Text, Boolean, Label.
            Self::Text(_) | Self::Boolean(_) | Self::Label(_) => None,
        }
    }

    /// Decoded text view of a string-typed property. Returns a
    /// `Cow<str>` so the common (no-escape) case avoids allocation
    /// while still emitting a properly unescaped string when the raw
    /// literal carries ISO 10303-21 escapes.
    ///
    /// Semantics:
    /// * `Text` / `Label` — `Some(Cow::Borrowed(s))`. Already decoded
    ///   strings; zero alloc.
    /// * `Other { raw, .. }` where `raw` is a quoted STEP literal
    ///   (`'...'`):
    ///     * If the quoted contents are escape-free, returns
    ///       `Some(Cow::Borrowed(inner))` — a slice into the stored
    ///       `raw`, zero alloc. This is the common case in practice
    ///       (`IfcDescriptiveMeasure`, `IfcGloballyUniqueId`, custom
    ///       strings rarely embed quotes or control characters).
    ///     * If the inner bytes contain `\` or `'` (the only two byte
    ///       markers that can trigger an ISO 10303-21 escape
    ///       sequence — `\\`, `\'`, `\n`, `\r`, `\t`, or the
    ///       doubled-quote `''`), returns
    ///       `Some(Cow::Owned(unescape_step_string(inner)))`. The
    ///       returned string is then byte-identical to what
    ///       `Text(...).as_text()` would return for the equivalent
    ///       decoded value (e.g. `Other` with raw `'O''Brien'` and
    ///       `Text("O'Brien")` both yield `"O'Brien"`). This
    ///       consistency matters for schedule / BOQ consumers that
    ///       merge string values across `Text`, `Label`, and `Other`
    ///       variants — without it, a property migrated from a
    ///       modelled `Text` to an opaque `Other` would suddenly
    ///       compare unequal to its original.
    /// * All other variants (numeric measures, booleans) return
    ///   `None`.
    pub fn as_text(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Text(s) | Self::Label(s) => Some(Cow::Borrowed(s.as_str())),
            Self::Other { raw, .. } => {
                let trimmed = raw.trim();
                let bytes = trimmed.as_bytes();
                if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
                    let inner = &trimmed[1..trimmed.len() - 1];
                    if has_step_escape(inner) {
                        Some(Cow::Owned(unescape_step_string(inner)))
                    } else {
                        Some(Cow::Borrowed(inner))
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Fast probe for byte markers that can trigger ISO 10303-21 string
/// escape decoding. Returns `true` iff the slice contains a `\` (which
/// could introduce `\\`, `\'`, `\n`, `\r`, or `\t`) or a `'` (which
/// could be the leading half of a doubled-quote `''`). Plain text
/// strings — overwhelmingly the common case for IFC measure values —
/// trip neither marker, so the caller can hand out a borrowed slice
/// directly.
fn has_step_escape(s: &str) -> bool {
    s.bytes().any(|b| b == b'\\' || b == b'\'')
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertySet {
    pub name: String,
    pub properties: BTreeMap<String, PropertyValue>,
}

impl PropertySet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: PropertyValue) -> &mut Self {
        self.properties.insert(key.into(), value);
        self
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("PropertySet serializes");
        blake3::hash(&bytes).into()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantitySet {
    pub name: String,
    pub quantities: BTreeMap<String, PropertyValue>,
}

impl QuantitySet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            quantities: BTreeMap::new(),
        }
    }
}

/// Standard IFC property-set templates. These define which keys are
/// expected for each class and the value type. The validator (Task 25)
/// uses these to flag missing required properties.
pub fn standard_pset_keys(class: &IfcClass) -> &'static [&'static str] {
    match class {
        IfcClass::IfcWall | IfcClass::IfcWallStandardCase => &[
            "Reference",
            "LoadBearing",
            "IsExternal",
            "ThermalTransmittance",
            "FireRating",
            "AcousticRating",
        ],
        IfcClass::IfcDoor => &[
            "Reference",
            "FireRating",
            "AcousticRating",
            "SecurityRating",
            "IsExternal",
            "ThermalTransmittance",
        ],
        IfcClass::IfcWindow => &[
            "Reference",
            "GlazingAreaFraction",
            "ThermalTransmittance",
            "Infiltration",
            "IsExternal",
        ],
        IfcClass::IfcSpace => &[
            "Reference",
            "Category",
            "PubliclyAccessible",
            "HandicapAccessible",
        ],
        IfcClass::IfcSlab => &[
            "Reference",
            "LoadBearing",
            "IsExternal",
            "ThermalTransmittance",
        ],
        _ => &[],
    }
}

/// The canonical Pset name for a given class (the "common" pset).
pub fn standard_pset_name_for_class(class: &IfcClass) -> Option<&'static str> {
    Some(match class {
        IfcClass::IfcWall | IfcClass::IfcWallStandardCase => "Pset_WallCommon",
        IfcClass::IfcDoor => "Pset_DoorCommon",
        IfcClass::IfcWindow => "Pset_WindowCommon",
        IfcClass::IfcSpace => "Pset_SpaceCommon",
        IfcClass::IfcSlab => "Pset_SlabCommon",
        IfcClass::IfcColumn => "Pset_ColumnCommon",
        IfcClass::IfcBeam => "Pset_BeamCommon",
        IfcClass::IfcRoof => "Pset_RoofCommon",
        IfcClass::IfcStair => "Pset_StairCommon",
        IfcClass::IfcRailing => "Pset_RailingCommon",
        _ => return None,
    })
}

/// Container for an element's property sets and quantity sets, split
/// by type-level (shared across instances) and instance-level
/// (per-element).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ElementProperties {
    /// Pset name → property set (instance-level).
    pub psets: BTreeMap<String, PropertySet>,
    /// Qto name → quantity set (instance-level).
    pub qsets: BTreeMap<String, QuantitySet>,
    /// IFC type-level psets: only one per IfcTypeObject. Shared by all
    /// instances that point at the same type.
    pub type_psets: BTreeMap<String, PropertySet>,
}

impl ElementProperties {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_pset(&mut self, pset: PropertySet) {
        self.psets.insert(pset.name.clone(), pset);
    }

    pub fn upsert_type_pset(&mut self, pset: PropertySet) {
        self.type_psets.insert(pset.name.clone(), pset);
    }

    pub fn upsert_qset(&mut self, qset: QuantitySet) {
        self.qsets.insert(qset.name.clone(), qset);
    }

    pub fn get(&self, pset_name: &str, key: &str) -> Option<&PropertyValue> {
        if let Some(p) = self.psets.get(pset_name) {
            if let Some(v) = p.properties.get(key) {
                return Some(v);
            }
        }
        if let Some(q) = self.qsets.get(pset_name) {
            if let Some(v) = q.quantities.get(key) {
                return Some(v);
            }
        }
        self.type_psets.get(pset_name)?.properties.get(key)
    }
}

/// Project-level property store, indexed by element id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyStore {
    entries: BTreeMap<EntityId, ElementProperties>,
}

impl PropertyStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entry(&mut self, id: EntityId) -> &mut ElementProperties {
        self.entries.entry(id).or_default()
    }

    pub fn get(&self, id: &EntityId) -> Option<&ElementProperties> {
        self.entries.get(id)
    }

    pub fn remove(&mut self, id: &EntityId) -> Option<ElementProperties> {
        self.entries.remove(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&EntityId, &ElementProperties)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_with_content() {
        let mut a = PropertySet::new("Pset_WallCommon");
        a.set("LoadBearing", PropertyValue::Boolean(false));
        let f1 = a.fingerprint();
        a.set("LoadBearing", PropertyValue::Boolean(true));
        let f2 = a.fingerprint();
        assert_ne!(f1, f2);
    }

    #[test]
    fn measure_type_strings() {
        assert_eq!(
            PropertyValue::Length(1.0).ifc_measure_type(),
            "IfcLengthMeasure"
        );
        assert_eq!(
            PropertyValue::Boolean(true).ifc_measure_type(),
            "IfcBoolean"
        );
        assert_eq!(
            PropertyValue::Label("x".into()).ifc_measure_type(),
            "IfcLabel"
        );
    }

    #[test]
    fn value_coercions() {
        assert_eq!(PropertyValue::Real(3.5).as_real(), Some(3.5));
        assert_eq!(PropertyValue::Integer(7).as_real(), Some(7.0));
        assert!(PropertyValue::Boolean(true).as_real().is_none());
        assert_eq!(
            PropertyValue::Text("hi".into()).as_text().as_deref(),
            Some("hi")
        );
        assert_eq!(
            PropertyValue::Label("hi".into()).as_text().as_deref(),
            Some("hi")
        );
    }

    /// `PropertyValue::Other` is the catch-all for opaque IFC measure
    /// types AEC Studio doesn't model natively. `as_real` must surface
    /// numeric raws so they reach BOQ / schedule consumers; `as_text`
    /// must surface quoted-string raws likewise. Booleans (`.T.`/`.F.`)
    /// and any other lexical shapes return `None` to match the typed
    /// variants' contract.
    #[test]
    fn other_variant_surfaces_numeric_and_string_raws_to_consumers() {
        let density = PropertyValue::Other {
            measure: "IFCMASSDENSITYMEASURE".into(),
            raw: "2400.0".into(),
        };
        assert_eq!(density.as_real(), Some(2400.0));
        assert!(density.as_text().is_none());

        let count = PropertyValue::Other {
            measure: "IFCCOUNTMEASURE".into(),
            raw: "12".into(),
        };
        assert_eq!(count.as_real(), Some(12.0));

        let scientific = PropertyValue::Other {
            measure: "IFCFREQUENCYMEASURE".into(),
            raw: "1.5e-3".into(),
        };
        assert_eq!(scientific.as_real(), Some(1.5e-3));

        // IFC's legacy FORTRAN-style `D` / `d` exponent literals
        // (`1.5D-3`, `2d5`) must parse the same as the canonical `e`
        // form. Pre-2010 AutoCAD-IFC / ARX exporters historically
        // emitted this variant — accepting it on the way in is what
        // makes the doc claim above true and keeps real-world
        // archives readable.
        let d_upper = PropertyValue::Other {
            measure: "IFCMASSDENSITYMEASURE".into(),
            raw: "1.5D-3".into(),
        };
        assert_eq!(d_upper.as_real(), Some(1.5e-3));
        let d_lower = PropertyValue::Other {
            measure: "IFCFREQUENCYMEASURE".into(),
            raw: "1.5d-3".into(),
        };
        assert_eq!(d_lower.as_real(), Some(1.5e-3));
        let d_integer_mantissa = PropertyValue::Other {
            measure: "IFCCOUNTMEASURE".into(),
            raw: "2D5".into(),
        };
        assert_eq!(d_integer_mantissa.as_real(), Some(2e5));

        let descriptive = PropertyValue::Other {
            measure: "IFCDESCRIPTIVEMEASURE".into(),
            raw: "'kg/m3'".into(),
        };
        assert_eq!(descriptive.as_text().as_deref(), Some("kg/m3"));
        // The escape-free common case must return Cow::Borrowed so we
        // pay nothing extra for typical IFC measure strings.
        assert!(matches!(descriptive.as_text(), Some(Cow::Borrowed(_))));
        assert_eq!(descriptive.as_real(), None);

        // STEP escape decoding: a string carrying ISO 10303-21 escapes
        // must come back fully decoded so consumers comparing across
        // `Text` and `Other` variants see byte-identical results.
        // Single-quote via the doubled-quote escape `''`.
        let with_apostrophe = PropertyValue::Other {
            measure: "IFCDESCRIPTIVEMEASURE".into(),
            raw: "'O''Brien'".into(),
        };
        assert_eq!(with_apostrophe.as_text().as_deref(), Some("O'Brien"));
        assert!(matches!(with_apostrophe.as_text(), Some(Cow::Owned(_))));
        assert_eq!(
            with_apostrophe.as_text().as_deref(),
            PropertyValue::Text("O'Brien".into()).as_text().as_deref(),
            "escape-decoded Other must compare equal to the modelled Text variant"
        );

        // Backslash escapes (`\\` for `\`, `\n` for newline) must
        // also decode.
        let with_control = PropertyValue::Other {
            measure: "IFCDESCRIPTIVEMEASURE".into(),
            raw: r"'a\\b\nc'".into(),
        };
        assert_eq!(with_control.as_text().as_deref(), Some("a\\b\nc"));
        assert!(matches!(with_control.as_text(), Some(Cow::Owned(_))));

        let boolean = PropertyValue::Other {
            measure: "IFCBOOLEAN".into(),
            raw: ".T.".into(),
        };
        assert_eq!(boolean.as_real(), None);
        assert!(boolean.as_text().is_none());

        // Whitespace tolerance: the writer never adds leading/trailing
        // whitespace, but a defensive caller hand-constructing the
        // variant might. Trim and still parse.
        let padded = PropertyValue::Other {
            measure: "IFCMASSDENSITYMEASURE".into(),
            raw: "  2400.0  ".into(),
        };
        assert_eq!(padded.as_real(), Some(2400.0));
    }

    #[test]
    fn standard_pset_for_wall_has_expected_keys() {
        let keys = standard_pset_keys(&IfcClass::IfcWall);
        assert!(keys.contains(&"LoadBearing"));
        assert!(keys.contains(&"FireRating"));
        assert_eq!(
            standard_pset_name_for_class(&IfcClass::IfcWall),
            Some("Pset_WallCommon")
        );
    }

    #[test]
    fn property_store_round_trip() {
        let mut store = PropertyStore::new();
        let e = EntityId::new();
        let mut p = PropertySet::new("Pset_WallCommon");
        p.set("LoadBearing", PropertyValue::Boolean(true));
        p.set("FireRating", PropertyValue::Label("EI60".into()));
        store.entry(e.clone()).upsert_pset(p);
        let got = store
            .get(&e)
            .unwrap()
            .get("Pset_WallCommon", "FireRating")
            .unwrap();
        assert_eq!(got.as_text().as_deref(), Some("EI60"));
    }

    #[test]
    fn instance_psets_shadow_type_psets() {
        let mut props = ElementProperties::new();
        let mut type_pset = PropertySet::new("Pset_WallCommon");
        type_pset.set("ThermalTransmittance", PropertyValue::Real(0.30));
        props.upsert_type_pset(type_pset);
        let mut inst = PropertySet::new("Pset_WallCommon");
        inst.set("ThermalTransmittance", PropertyValue::Real(0.18));
        props.upsert_pset(inst);
        assert_eq!(
            props
                .get("Pset_WallCommon", "ThermalTransmittance")
                .unwrap()
                .as_real(),
            Some(0.18)
        );
    }
}
