//! IFC4 material library: `IfcMaterial`, `IfcMaterialLayer`,
//! `IfcMaterialLayerSet`, and `IfcRelAssociatesMaterial`.
//!
//! Holds the project-side material graph as a sibling to
//! [`crate::properties::PropertyStore`]: elements gain an optional
//! [`MaterialAssignment`] that points either at a single
//! [`Material`] or at a composite [`MaterialLayerSet`]. Materials and
//! layer-sets are deduplicated by name so the same concrete-on-
//! a-storey assignment doesn't generate one `IfcMaterial` STEP entity
//! per wall.
//!
//! ### Why a dedicated store and not Psets?
//!
//! IfcOpenShell, IfcQuery, and most BIM tooling read element material
//! by walking the `IfcRelAssociatesMaterial` graph — *not* by
//! reading a string property named `"Material"` off Pset_WallCommon.
//! Carrying material as a Pset key lost the layer-thickness data
//! (wall-build-up) and forced consumers to special-case our exporter.
//! With a real `MaterialStore`, the writer emits proper
//! `IFCMATERIAL` / `IFCMATERIALLAYERSET` / `IFCRELASSOCIATESMATERIAL`
//! records, the reader recovers them, and downstream BIM tooling
//! (Revit, ArchiCAD, IfcOpenShell) sees the same structure that any
//! other authoring tool would have written.
//!
//! ### Schema reach
//!
//! IFC4 by default. IFC2x3 accepts a 1-arg `IfcMaterial('Name')` form
//! (no description/category); the reader handles both. IFC4x3 added
//! `IfcMaterialConstituent` / `IfcMaterialConstituentSet` and
//! `IfcMaterialProfileSet` — those are out of scope for this layer and
//! preserved verbatim by the property roundtrip path when encountered.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

/// A single homogeneous material — e.g. `"Concrete - C25/30"`.
///
/// Two materials with the same name are considered the same material
/// regardless of description / category. This matches the IFC4
/// `IfcMaterial.Name` uniqueness convention used by Revit, ArchiCAD,
/// and IfcOpenShell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Material {
    /// IFC4 `IfcMaterial.Name` — the load-bearing identity. Empty
    /// names are rejected by [`MaterialStore::upsert_material`].
    pub name: String,
    /// IFC4 `IfcMaterial.Description` (optional).
    pub description: Option<String>,
    /// IFC4 `IfcMaterial.Category` — e.g. `"concrete"`, `"steel"`,
    /// `"glazing"`. Used downstream for BIM filters and BoQ
    /// classification.
    pub category: Option<String>,
}

impl Material {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            category: None,
        }
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// A single layer in a layered wall / slab / roof / covering build-up.
///
/// The IFC4 schema fields preserved here are exactly the ones AEC
/// Studio needs for round-trip + BoQ + drawing generation:
/// the parent material, layer thickness, ventilation flag, and the
/// optional name / description / category / priority. Other IFC4
/// fields (`IfcMaterialLayer.LayerSetName`, etc.) are derived from
/// the parent [`MaterialLayerSet`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialLayer {
    /// Index into the parent [`MaterialLayerSet::layers`]. Stored as
    /// a `String` name rather than a numeric index because IFC4
    /// `IfcMaterialLayer.Material` references the
    /// [`Material::name`] directly.
    pub material_name: String,
    /// Layer thickness in metres. IFC4
    /// `IfcMaterialLayer.LayerThickness` is in the project length
    /// unit; AEC Studio normalises to metres on import and on
    /// export.
    pub thickness_m: f64,
    /// IFC4 `IfcMaterialLayer.IsVentilated`. None when the layer
    /// is non-ventilated (the IFC4 default).
    pub is_ventilated: Option<bool>,
    /// IFC4 `IfcMaterialLayer.Name`.
    pub name: Option<String>,
    /// IFC4 `IfcMaterialLayer.Description`.
    pub description: Option<String>,
    /// IFC4 `IfcMaterialLayer.Category`.
    pub category: Option<String>,
    /// IFC4 `IfcMaterialLayer.Priority` (0-100). None when not
    /// authored.
    pub priority: Option<i32>,
}

impl MaterialLayer {
    pub fn new(material_name: impl Into<String>, thickness_m: f64) -> Self {
        Self {
            material_name: material_name.into(),
            thickness_m,
            is_ventilated: None,
            name: None,
            description: None,
            category: None,
            priority: None,
        }
    }
}

/// A composite wall / slab / roof build-up: an ordered stack of
/// [`MaterialLayer`]s.
///
/// Layer order matters — IFC4 `IfcMaterialLayerSet.MaterialLayers` is
/// an ordered list from one side of the element to the other (e.g.
/// outside → inside for a wall). The writer preserves the order on
/// export and the reader preserves it on import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialLayerSet {
    /// IFC4 `IfcMaterialLayerSet.LayerSetName` — the load-bearing
    /// identity. Two layer-sets with the same name are considered the
    /// same composite.
    pub name: String,
    /// IFC4 `IfcMaterialLayerSet.Description`.
    pub description: Option<String>,
    /// Ordered list of layers (outside → inside for walls; top →
    /// bottom for slabs).
    pub layers: Vec<MaterialLayer>,
}

impl MaterialLayerSet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            layers: Vec::new(),
        }
    }

    pub fn with_layer(mut self, layer: MaterialLayer) -> Self {
        self.layers.push(layer);
        self
    }

    /// Total build-up thickness, in metres.
    pub fn total_thickness_m(&self) -> f64 {
        self.layers.iter().map(|l| l.thickness_m).sum()
    }
}

/// What an element is made of: either a single [`Material`] or a
/// composite [`MaterialLayerSet`].
///
/// On IFC export this maps onto an `IfcRelAssociatesMaterial` whose
/// `RelatingMaterial` is either an `IfcMaterial` (single) or an
/// `IfcMaterialLayerSet` (layered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialAssignment {
    /// Element is homogeneously made of the named [`Material`].
    Single(String),
    /// Element is a composite of the named [`MaterialLayerSet`].
    LayerSet(String),
}

impl MaterialAssignment {
    /// The load-bearing reference name on either side of the enum,
    /// used by the writer to look up the corresponding STEP id and
    /// emit the `IfcRelAssociatesMaterial` record.
    pub fn reference_name(&self) -> &str {
        match self {
            Self::Single(n) | Self::LayerSet(n) => n.as_str(),
        }
    }
}

/// Project-level material store: holds all materials and layer-sets
/// known to the project plus the per-element assignment table.
///
/// Mirrors the shape of [`crate::properties::PropertyStore`] and
/// [`crate::classification::ClassificationStore`] so the BIM facade
/// stays consistent. Use [`upsert_material`] / [`upsert_layer_set`]
/// to add definitions, and [`assign_to_element`] / [`assignment`] to
/// query the per-element binding.
///
/// [`upsert_material`]: MaterialStore::upsert_material
/// [`upsert_layer_set`]: MaterialStore::upsert_layer_set
/// [`assign_to_element`]: MaterialStore::assign_to_element
/// [`assignment`]: MaterialStore::assignment
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialStore {
    materials: BTreeMap<String, Material>,
    layer_sets: BTreeMap<String, MaterialLayerSet>,
    assignments: BTreeMap<EntityId, MaterialAssignment>,
}

impl MaterialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a material, keyed on [`Material::name`].
    /// Returns the previous value if a same-named material was
    /// already present. An empty name is rejected and returns the
    /// new material unchanged.
    pub fn upsert_material(&mut self, mat: Material) -> Option<Material> {
        if mat.name.is_empty() {
            return None;
        }
        self.materials.insert(mat.name.clone(), mat)
    }

    /// Insert (or replace) a layer-set, keyed on
    /// [`MaterialLayerSet::name`]. Returns the previous value if a
    /// same-named layer-set was already present. An empty name is
    /// rejected.
    pub fn upsert_layer_set(&mut self, set: MaterialLayerSet) -> Option<MaterialLayerSet> {
        if set.name.is_empty() {
            return None;
        }
        self.layer_sets.insert(set.name.clone(), set)
    }

    /// Assign a [`MaterialAssignment`] to an element. The named
    /// material / layer-set must already be in the store, otherwise
    /// the assignment is silently dropped (returns `false`).
    pub fn assign_to_element(&mut self, id: EntityId, assignment: MaterialAssignment) -> bool {
        let known = match &assignment {
            MaterialAssignment::Single(name) => self.materials.contains_key(name),
            MaterialAssignment::LayerSet(name) => self.layer_sets.contains_key(name),
        };
        if !known {
            return false;
        }
        self.assignments.insert(id, assignment);
        true
    }

    pub fn material(&self, name: &str) -> Option<&Material> {
        self.materials.get(name)
    }

    pub fn layer_set(&self, name: &str) -> Option<&MaterialLayerSet> {
        self.layer_sets.get(name)
    }

    pub fn assignment(&self, id: &EntityId) -> Option<&MaterialAssignment> {
        self.assignments.get(id)
    }

    pub fn materials(&self) -> impl Iterator<Item = (&String, &Material)> {
        self.materials.iter()
    }

    pub fn layer_sets(&self) -> impl Iterator<Item = (&String, &MaterialLayerSet)> {
        self.layer_sets.iter()
    }

    pub fn assignments(&self) -> impl Iterator<Item = (&EntityId, &MaterialAssignment)> {
        self.assignments.iter()
    }

    pub fn material_count(&self) -> usize {
        self.materials.len()
    }

    pub fn layer_set_count(&self) -> usize {
        self.layer_sets.len()
    }

    pub fn assignment_count(&self) -> usize {
        self.assignments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty() && self.layer_sets.is_empty() && self.assignments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_material_rejects_empty_name() {
        let mut store = MaterialStore::new();
        let inserted = store.upsert_material(Material::new(""));
        assert!(inserted.is_none());
        assert_eq!(store.material_count(), 0);
    }

    #[test]
    fn upsert_material_replaces_same_name() {
        let mut store = MaterialStore::new();
        store.upsert_material(Material::new("Concrete").with_category("concrete"));
        let prev = store.upsert_material(Material::new("Concrete").with_category("masonry"));
        let prev = prev.expect("previous material returned on replace");
        assert_eq!(prev.category.as_deref(), Some("concrete"));
        assert_eq!(
            store.material("Concrete").unwrap().category.as_deref(),
            Some("masonry")
        );
        assert_eq!(store.material_count(), 1);
    }

    #[test]
    fn layer_set_preserves_order_and_total_thickness() {
        let set = MaterialLayerSet::new("Exterior Wall - 250 mm")
            .with_layer(MaterialLayer::new("Concrete", 0.150))
            .with_layer(MaterialLayer::new("Insulation", 0.080))
            .with_layer(MaterialLayer::new("Gypsum", 0.020));
        let names: Vec<&str> = set
            .layers
            .iter()
            .map(|l| l.material_name.as_str())
            .collect();
        assert_eq!(names, vec!["Concrete", "Insulation", "Gypsum"]);
        assert!((set.total_thickness_m() - 0.250).abs() < 1e-9);
    }

    #[test]
    fn assign_to_element_rejects_unknown_material_and_layer_set() {
        let mut store = MaterialStore::new();
        let id = EntityId::new();
        // No materials registered yet — assignment must fail.
        let ok = store.assign_to_element(id.clone(), MaterialAssignment::Single("Concrete".into()));
        assert!(!ok);
        assert!(store.assignment(&id).is_none());
        // Register a layer-set, but assign a different layer-set —
        // must still fail because the named one isn't present.
        store.upsert_layer_set(MaterialLayerSet::new("LS-1"));
        let ok = store.assign_to_element(id.clone(), MaterialAssignment::LayerSet("LS-2".into()));
        assert!(!ok);
        assert!(store.assignment(&id).is_none());
    }

    #[test]
    fn assign_to_element_accepts_known_material() {
        let mut store = MaterialStore::new();
        store.upsert_material(Material::new("Concrete"));
        let id = EntityId::new();
        let ok = store.assign_to_element(id.clone(), MaterialAssignment::Single("Concrete".into()));
        assert!(ok);
        assert!(matches!(
            store.assignment(&id),
            Some(MaterialAssignment::Single(name)) if name == "Concrete"
        ));
    }

    #[test]
    fn assign_to_element_accepts_known_layer_set() {
        let mut store = MaterialStore::new();
        store.upsert_layer_set(
            MaterialLayerSet::new("Wall-200").with_layer(MaterialLayer::new("Concrete", 0.200)),
        );
        let id = EntityId::new();
        let ok =
            store.assign_to_element(id.clone(), MaterialAssignment::LayerSet("Wall-200".into()));
        assert!(ok);
        assert_eq!(store.assignment_count(), 1);
    }

    #[test]
    fn material_assignment_reference_name_returns_inner_name() {
        let single = MaterialAssignment::Single("X".into());
        let composite = MaterialAssignment::LayerSet("Y".into());
        assert_eq!(single.reference_name(), "X");
        assert_eq!(composite.reference_name(), "Y");
    }

    #[test]
    fn material_store_serde_round_trips() {
        let mut store = MaterialStore::new();
        store.upsert_material(Material::new("Concrete").with_category("concrete"));
        store.upsert_layer_set(
            MaterialLayerSet::new("LS").with_layer(MaterialLayer::new("Concrete", 0.150)),
        );
        let id = EntityId::new();
        store.assign_to_element(id.clone(), MaterialAssignment::LayerSet("LS".into()));

        let json = serde_json::to_string(&store).expect("serialize");
        let round_trip: MaterialStore = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_trip, store);
        assert_eq!(
            round_trip.assignment(&id).cloned().unwrap(),
            MaterialAssignment::LayerSet("LS".into())
        );
    }
}
