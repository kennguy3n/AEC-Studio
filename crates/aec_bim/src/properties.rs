//! Property sets (Pset_*), quantity sets (Qto_*), and custom psets.
//!
//! Property values are typed; the BIM cache fingerprints them with BLAKE3 so
//! we can detect changes when re-importing an IFC.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::IfcClass;

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
    /// `IfcCountMeasure`). The reader stores the original measure
    /// tag (without the `IFC` prefix and case-normalised, e.g.
    /// `"MassDensityMeasure"`) and the raw STEP value literal so
    /// the writer can round-trip the property losslessly without
    /// having to enumerate every IFC measure type in this enum.
    Other {
        /// IFC measure-type name (e.g. `"IfcMassDensityMeasure"`).
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
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) | Self::Label(s) => Some(s.as_str()),
            _ => None,
        }
    }
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
        assert_eq!(PropertyValue::Text("hi".into()).as_text(), Some("hi"));
        assert_eq!(PropertyValue::Label("hi".into()).as_text(), Some("hi"));
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
        assert_eq!(got.as_text(), Some("EI60"));
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
