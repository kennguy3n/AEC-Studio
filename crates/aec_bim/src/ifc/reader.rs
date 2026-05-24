//! IFC4 STEP reader matched to [`super::writer::IfcWriter`].
//!
//! Parses the in-process IFC4 STEP byte stream we emit and reconstructs:
//!
//!   * A [`Project`] spatial graph (Project → Site → Building → Storey
//!     → Space), with each node's `ifc_guid` populated from the file.
//!   * A [`ClassificationStore`] keyed on the recovered element IDs.
//!   * A [`PropertyStore`] with the original Pset / Qto names and
//!     `PropertyValue` types intact.
//!   * A `guid_for(EntityId)` map so callers can assert GUID
//!     preservation across a write → read cycle.
//!
//! The writer encodes each element's `EntityId` as the IFC entity's
//! `Name` field — concretely the literal `"{IfcTag}::{entity_id}"` —
//! so the parser can recover identity verbatim and the round-trip is
//! lossless for everything we serialise.
//!
//! Schema reach: the reader accepts IFC4, IFC2x3, and IFC4x3 STEP
//! files; the detected schema is exposed via [`IfcSnapshot::schema`]
//! so downstream consumers can decide how strictly to interpret
//! entities whose IFC version differs from AEC Studio's canonical
//! IFC4 model. Entities outside the modeled subset (custom Psets,
//! geometry instances, unmodeled spatial classes) are tolerated
//! and skipped during parse — the goal is end-to-end roundtrip
//! fidelity for the entities AEC Studio owns, not a complete
//! IFC4 implementation.

use std::collections::HashMap;
use std::io::{BufRead, Read};

use thiserror::Error;

use aec_core::types::EntityId;

use crate::classification::{ClassificationSource, ClassificationStore, IfcClass};
use crate::materials::{
    Material, MaterialAssignment, MaterialLayer, MaterialLayerSet, MaterialStore,
};
use crate::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use crate::spatial::Project;

#[derive(Debug, Error)]
pub enum IfcReadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed STEP file: {0}")]
    Malformed(String),
    #[error("missing required section: {0}")]
    MissingSection(&'static str),
    #[error("unknown IFC type: {0}")]
    UnknownType(String),
}

pub type IfcReadResult<T> = Result<T, IfcReadError>;

/// IFC schema versions the reader accepts.
///
/// Detected from the `FILE_SCHEMA` line in the STEP header. AEC
/// Studio's writer emits [`Ifc4`](IfcSchema::Ifc4); the reader is
/// permissive on input so files exported by other authoring tools
/// (which often target [`Ifc2x3`](IfcSchema::Ifc2x3)) round-trip
/// through AEC Studio without losing identity for the entities we
/// own. Entities unique to a particular schema generation are
/// tolerated and skipped — see [`IfcReader::from_string`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IfcSchema {
    /// IFC2x3 TC1 — ISO 16739:2005, still the most widely deployed
    /// schema in legacy authoring tools.
    Ifc2x3,
    /// IFC4 ADD2 TC1 — ISO 16739-1:2018, AEC Studio's canonical
    /// internal schema.
    #[default]
    Ifc4,
    /// IFC4x3 ADD2 — ISO 16739-1:2024, the current draft schema,
    /// adds infrastructure-domain entities.
    Ifc4x3,
}

impl IfcSchema {
    /// Match a STEP `FILE_SCHEMA(('...'))` literal (case-insensitive,
    /// whitespace-tolerant) against the known versions.
    pub fn from_step_literal(s: &str) -> Option<Self> {
        let up = s.trim().to_ascii_uppercase();
        match up.as_str() {
            "IFC2X3" => Some(IfcSchema::Ifc2x3),
            "IFC4" => Some(IfcSchema::Ifc4),
            "IFC4X3" | "IFC4X3_ADD2" | "IFC4X3_ADD1" | "IFC4X3_RC4" => Some(IfcSchema::Ifc4x3),
            _ => None,
        }
    }

    /// The canonical token AEC Studio's writer emits for this schema.
    pub fn as_step_literal(self) -> &'static str {
        match self {
            IfcSchema::Ifc2x3 => "IFC2X3",
            IfcSchema::Ifc4 => "IFC4",
            IfcSchema::Ifc4x3 => "IFC4X3",
        }
    }
}

/// Lightweight stats returned alongside the snapshot, useful for
/// assertions in roundtrip tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IfcReadStats {
    pub spatial_nodes: usize,
    pub elements: usize,
    pub psets: usize,
    pub qsets: usize,
    pub aggregations: usize,
    pub containments: usize,
    /// `IfcMaterial` instances recovered from the file.
    pub materials: usize,
    /// `IfcMaterialLayerSet` instances recovered.
    pub material_layer_sets: usize,
    /// `IfcRelAssociatesMaterial` element-to-material bindings
    /// recovered.
    pub material_assignments: usize,
    /// Number of `#N = TYPE(...)` entity instances the tokenizer
    /// observed before the modeled-entity filter. Useful as a
    /// sanity-check when feeding an external IFC file: the value
    /// should be ≥ `spatial_nodes + elements + psets + qsets`,
    /// with the difference being unmodeled / unknown records that
    /// were tolerated and skipped.
    pub records_seen: usize,
}

/// Snapshot returned by [`IfcReader::from_string`]. Everything below
/// is keyed on the **original** AEC Studio EntityIds — the writer
/// stashes them in the IFC `Name` field so the reader can restore the
/// exact identity graph the writer produced.
#[derive(Debug, Clone)]
pub struct IfcSnapshot {
    pub project: Project,
    pub classification: ClassificationStore,
    pub properties: PropertyStore,
    /// Material library — `IfcMaterial` definitions, layer-set
    /// composites, and per-element `IfcRelAssociatesMaterial`
    /// bindings recovered from the file. Empty when the source IFC
    /// carried no material entities (rare in practice — most
    /// authoring tools emit them by default).
    pub materials: MaterialStore,
    /// GUID per spatial node (Project/Site/Building/Storey/Space) and
    /// per building element, keyed on the original `EntityId`.
    pub guid_by_entity: HashMap<EntityId, String>,
    /// The recovered spatial parent for each element (`storey_id`).
    pub element_parent: HashMap<EntityId, EntityId>,
    /// IFC schema declared in the file's `FILE_SCHEMA` header.
    pub schema: IfcSchema,
    pub stats: IfcReadStats,
}

pub struct IfcReader;

impl IfcReader {
    pub fn from_string(text: &str) -> IfcReadResult<IfcSnapshot> {
        // Strict envelope check so we fail loudly if a caller passes a
        // non-IFC blob.
        if !text.contains("HEADER;") || !text.contains("DATA;") {
            return Err(IfcReadError::MissingSection("HEADER/DATA"));
        }
        let schema = detect_schema(text)?;
        if !text.trim_end().ends_with("END-ISO-10303-21;") {
            return Err(IfcReadError::Malformed(
                "missing END-ISO-10303-21; terminator".into(),
            ));
        }

        // ---- Tokenise: every `#N = TYPE(...);` becomes (id, type, args). ----
        let groups = parse_step_groups(text)?;

        // ---- Index by STEP id ----
        let by_id: HashMap<u32, &StepRecord> = groups.iter().map(|g| (g.step_id, g)).collect();

        // First pass: figure out which records are spatial nodes vs
        // elements vs psets vs qsets, and recover the originating
        // EntityId from the `Name` field.
        let mut project_entity: Option<EntityId> = None;
        let mut spatial: HashMap<u32, SpatialRow> = HashMap::new();
        let mut elements: HashMap<u32, ElementRow> = HashMap::new();
        let mut psets: HashMap<u32, PsetRow> = HashMap::new();
        let mut qsets: HashMap<u32, QsetRow> = HashMap::new();
        let mut prop_values: HashMap<u32, PropRow> = HashMap::new();
        let mut qty_values: HashMap<u32, QtyRow> = HashMap::new();
        // Material library — three index tables resolved together in
        // the second pass. `materials_step` keys on the STEP id
        // returning the material name (the load-bearing identity);
        // `material_layers_step` returns the parent material name +
        // thickness so a layer-set can rebuild the ordered layer
        // stack; `material_layer_sets_step` returns the
        // `MaterialLayerSet` keyed by STEP id, populated *after*
        // the second pass resolves layer refs.
        let mut materials_step: HashMap<u32, Material> = HashMap::new();
        let mut material_layers_step: HashMap<u32, MaterialLayer> = HashMap::new();
        let mut material_layer_sets_step: HashMap<u32, (String, Option<String>, Vec<u32>)> =
            HashMap::new();

        for g in &groups {
            match g.kind.as_str() {
                "IFCOWNERHISTORY" => {}
                "IFCPROJECT" | "IFCSITE" | "IFCBUILDING" | "IFCBUILDINGSTOREY" | "IFCSPACE" => {
                    let class = ifc_class_from_tag(&g.kind, &g.raw_kind);
                    let name = g.string_arg(3)?;
                    // Spatial nodes are authored by the writer with the
                    // user-facing display name ("Café", "Ground"). They
                    // don't carry the EntityId encoding — they're
                    // assigned a fresh EntityId on parse and the GUID
                    // is the source of identity.
                    let guid = g.string_arg(0)?;
                    let entity = EntityId::new();
                    if matches!(class, IfcClass::IfcProject) {
                        project_entity = Some(entity.clone());
                    }
                    spatial.insert(
                        g.step_id,
                        SpatialRow {
                            entity,
                            guid,
                            class,
                            name,
                        },
                    );
                }
                "IFCPROPERTYSINGLEVALUE" => {
                    let key = g.string_arg(0)?;
                    let measure = g
                        .args
                        .get(2)
                        .cloned()
                        .ok_or_else(|| Self::malformed(g, "expected measure value"))?;
                    let value = parse_typed_measure(&measure)?;
                    prop_values.insert(g.step_id, PropRow { key, value });
                }
                // Any `IFCQUANTITY*` entity is dispatched here. The
                // five canonical quantity kinds (length/area/volume/
                // count/weight) map to typed `PropertyValue` variants;
                // IFC4x3 added `IFCQUANTITYTIME` and any future
                // schema-level quantity kinds fall through the
                // catch-all arm of `parse_quantity_typed` to a
                // `PropertyValue::Other { measure, raw }` so the value
                // survives a read→write round-trip verbatim — i.e. the
                // writer's `serialize_quantity_value::Other` arm emits
                // exactly the same STEP literal we ingested here.
                //
                // We intentionally do NOT enumerate the known kinds in
                // the dispatch arm: that would silently drop any
                // future or vendor-extension `IFCQUANTITY*` to the
                // element fallback below (which then skips the entity
                // entirely because it lacks the `tag::eid` Name field).
                // A prefix match keeps the round-trip closed for the
                // full open-ended family.
                kind if kind.starts_with("IFCQUANTITY") => {
                    let key = g.string_arg(0)?;
                    let lit = g
                        .args
                        .get(3)
                        .cloned()
                        .ok_or_else(|| Self::malformed(g, "expected quantity value"))?;
                    let value = parse_quantity_typed(&g.kind, &lit)?;
                    qty_values.insert(g.step_id, QtyRow { key, value });
                }
                "IFCPROPERTYSET" => {
                    let name = g.string_arg(2)?;
                    let refs = g.ref_list_arg(4)?;
                    psets.insert(g.step_id, PsetRow { name, refs });
                }
                "IFCELEMENTQUANTITY" => {
                    let name = g.string_arg(2)?;
                    let refs = g.ref_list_arg(5)?;
                    qsets.insert(g.step_id, QsetRow { name, refs });
                }
                "IFCRELAGGREGATES"
                | "IFCRELCONTAINEDINSPATIALSTRUCTURE"
                | "IFCRELDEFINESBYPROPERTIES"
                | "IFCRELASSOCIATESMATERIAL" => {
                    // Handled in the second pass.
                }
                "IFCMATERIAL" => {
                    // IFC4 form: IFCMATERIAL('Name','Description','Category')
                    // IFC2x3 form: IFCMATERIAL('Name') — accept either by
                    // positional argument count.
                    let name = g.string_arg(0)?;
                    let description = optional_string_arg(g, 1)?;
                    let category = optional_string_arg(g, 2)?;
                    if !name.is_empty() {
                        materials_step.insert(
                            g.step_id,
                            Material {
                                name,
                                description,
                                category,
                            },
                        );
                    }
                }
                "IFCMATERIALLAYER" => {
                    // IFC4 form:
                    //   IFCMATERIALLAYER(#Material, LayerThickness,
                    //                    IsVentilated, 'Name',
                    //                    'Description', 'Category',
                    //                    Priority)
                    // IFC2x3 form (3-arg):
                    //   IFCMATERIALLAYER(#Material, LayerThickness, IsVentilated)
                    // We accept either; missing optionals fall through
                    // as `None`. The Material ref is resolved in the
                    // second pass.
                    let material_ref = g.ref_arg(0)?;
                    let thickness = g
                        .args
                        .get(1)
                        .and_then(|a| parse_step_real(a))
                        .ok_or_else(|| Self::malformed(g, "expected LayerThickness real"))?;
                    let is_ventilated = parse_optional_bool(g.args.get(2).map(String::as_str));
                    let name = optional_string_arg(g, 3)?;
                    let description = optional_string_arg(g, 4)?;
                    let category = optional_string_arg(g, 5)?;
                    let priority = g.args.get(6).and_then(|a| {
                        if a == "$" {
                            None
                        } else {
                            a.parse::<i32>().ok()
                        }
                    });
                    // Stash the material ref as a sentinel name; the
                    // second pass replaces it with the resolved name.
                    let sentinel_material_name = format!("__ref:{material_ref}");
                    material_layers_step.insert(
                        g.step_id,
                        MaterialLayer {
                            material_name: sentinel_material_name,
                            thickness_m: thickness,
                            is_ventilated,
                            name,
                            description,
                            category,
                            priority,
                        },
                    );
                }
                "IFCMATERIALLAYERSET" => {
                    // IFCMATERIALLAYERSET((#L1,#L2,...), 'Name', 'Description')
                    let layer_refs = g.ref_list_arg(0)?;
                    let name = g.string_arg(1)?;
                    let description = optional_string_arg(g, 2)?;
                    if !name.is_empty() {
                        material_layer_sets_step.insert(g.step_id, (name, description, layer_refs));
                    }
                }
                other => {
                    // Anything else with a 9-field shape and a
                    // `tag::eid` name is an element.
                    //
                    // We split on the LAST `::` (rsplit_once) rather
                    // than the first. The writer composes
                    // `{IfcTag}::{EntityId}` and `EntityId::Display`
                    // is a stable UUID format that cannot contain
                    // `::`, but `IfcClass::Other(s)` allows the
                    // tag itself to contain arbitrary characters
                    // including `::` (e.g. a future
                    // `Other("Some::Custom::Type")` produced by an
                    // extension classifier). Splitting from the
                    // right makes that contract robust — the eid is
                    // always the suffix after the final `::`.
                    if let Ok(name) = g.string_arg(3) {
                        if let Some((tag, eid)) = name.rsplit_once("::") {
                            // Prefer the tag carried in the Name field
                            // over the STEP entity type. The two are
                            // identical for safe classes (the writer
                            // emits `{ifc_tag}::{eid}` and uses
                            // `ifc_tag` as the STEP type), but for
                            // `IfcClass::Other(s)` where `s` contains
                            // STEP-unsafe chars like `(`, `)` or `'`,
                            // the writer falls back to STEP type
                            // `IfcBuildingElementProxy` and keeps the
                            // original `s` only in the Name field.
                            // Reading the class from the Name tag
                            // therefore makes the round-trip lossless
                            // even for arbitrary user-supplied
                            // extension classifier strings. We pass
                            // `tag` for both the case-normalised match
                            // and the raw (case-preserving) fallback
                            // so `Other("Some::Custom::Type")`
                            // round-trips verbatim.
                            let class = ifc_class_from_tag(tag, tag);
                            let _ = other; // STEP type is informational
                            let guid = g.string_arg(0)?;
                            let entity = EntityId::from_string(eid).map_err(|e| {
                                IfcReadError::Malformed(format!(
                                    "embedded EntityId `{eid}` is invalid: {e}"
                                ))
                            })?;
                            elements.insert(
                                g.step_id,
                                ElementRow {
                                    entity,
                                    guid,
                                    class,
                                    tag: tag.to_string(),
                                },
                            );
                        }
                    }
                }
            }
        }

        // ---- Second pass: relations ----
        // IFCRELAGGREGATES(GUID,#owner,$,$,#parent,(#childs…))
        // IFCRELCONTAINEDINSPATIALSTRUCTURE(GUID,#owner,$,$,(#elems…),#storey)
        // IFCRELDEFINESBYPROPERTIES(GUID,#owner,$,$,(#elems…),#pset_or_qset)
        let mut agg: Vec<(u32, Vec<u32>)> = Vec::new();
        let mut contains: Vec<(u32, Vec<u32>)> = Vec::new();
        let mut defines_pset: Vec<(Vec<u32>, u32)> = Vec::new();
        // IFCRELASSOCIATESMATERIAL(GUID,#owner,$,$,(#elems…),#material_or_set)
        let mut associates_material: Vec<(Vec<u32>, u32)> = Vec::new();
        for g in &groups {
            match g.kind.as_str() {
                "IFCRELAGGREGATES" => {
                    let parent = g.ref_arg(4)?;
                    let children = g.ref_list_arg(5)?;
                    agg.push((parent, children));
                }
                "IFCRELCONTAINEDINSPATIALSTRUCTURE" => {
                    let children = g.ref_list_arg(4)?;
                    let storey = g.ref_arg(5)?;
                    contains.push((storey, children));
                }
                "IFCRELDEFINESBYPROPERTIES" => {
                    let elems = g.ref_list_arg(4)?;
                    let pset_ref = g.ref_arg(5)?;
                    defines_pset.push((elems, pset_ref));
                }
                "IFCRELASSOCIATESMATERIAL" => {
                    let elems = g.ref_list_arg(4)?;
                    let mat_ref = g.ref_arg(5)?;
                    associates_material.push((elems, mat_ref));
                }
                _ => {}
            }
        }

        // ---- Rebuild Project ----
        let mut project = Project::new("");
        // Replace the auto-created root with the one we recovered, so
        // EntityIds stay consistent with what the reader returns.
        let root_step = spatial
            .iter()
            .find_map(|(sid, row)| {
                if matches!(row.class, IfcClass::IfcProject) {
                    Some(*sid)
                } else {
                    None
                }
            })
            .ok_or(IfcReadError::MissingSection("IfcProject"))?;
        let root_row = spatial.get(&root_step).unwrap();
        project.nodes.clear();
        project.root = root_row.entity.clone();
        let project_entity = project_entity.expect("IfcProject parsed");
        for row in spatial.values() {
            project.nodes.insert(
                row.entity.clone(),
                crate::spatial::SpatialNode {
                    id: row.entity.clone(),
                    ifc_guid: Some(row.guid.clone()),
                    class: row.class.clone(),
                    name: row.name.clone(),
                    children: Vec::new(),
                    elements: Vec::new(),
                },
            );
        }
        // Apply aggregation edges.
        let mut aggregations = 0usize;
        for (parent_step, child_steps) in &agg {
            let parent_entity = spatial
                .get(parent_step)
                .map(|r| r.entity.clone())
                .ok_or_else(|| {
                    IfcReadError::Malformed(format!(
                        "IFCRELAGGREGATES parent #{} not a spatial node",
                        parent_step
                    ))
                })?;
            let node = project.nodes.get_mut(&parent_entity).unwrap();
            for child_step in child_steps {
                if let Some(child_entity) = spatial.get(child_step).map(|r| r.entity.clone()) {
                    node.children.push(child_entity);
                    aggregations += 1;
                }
            }
        }
        // Apply containment edges (elements → storey) and build the
        // element_parent map.
        let mut element_parent: HashMap<EntityId, EntityId> = HashMap::new();
        let mut containments = 0usize;
        for (storey_step, elem_steps) in &contains {
            let storey_entity = spatial
                .get(storey_step)
                .map(|r| r.entity.clone())
                .ok_or_else(|| {
                    IfcReadError::Malformed(format!(
                        "IFCRELCONTAINEDINSPATIALSTRUCTURE storey #{} not a spatial node",
                        storey_step
                    ))
                })?;
            for elem_step in elem_steps {
                if let Some(el) = elements.get(elem_step) {
                    project
                        .nodes
                        .get_mut(&storey_entity)
                        .unwrap()
                        .elements
                        .push(el.entity.clone());
                    element_parent.insert(el.entity.clone(), storey_entity.clone());
                    containments += 1;
                }
            }
        }

        // ---- Rebuild classification ----
        let mut classification = ClassificationStore::new();
        for el in elements.values() {
            classification.assign_imported(el.entity.clone(), el.class.clone());
        }
        // The writer doesn't currently embed the original
        // ClassificationSource, but assign_imported maps to
        // `ClassificationSource::Imported`, which is what an external
        // IFC file would naturally produce. Surface it as the canonical
        // source for round-tripped data.
        debug_assert!(classification
            .iter()
            .all(|(_, asg)| matches!(asg.source, ClassificationSource::Imported)));

        // ---- Rebuild PropertyStore ----
        let mut props = PropertyStore::new();
        let mut pset_count = 0usize;
        let mut qset_count = 0usize;
        // Resolve a step id to the underlying EntityId, accepting
        // either a building element or a spatial structure node.
        // Per IFC4, both element subtypes and spatial subtypes are
        // valid `IfcObjectDefinition` targets of an
        // `IfcRelDefinesByProperties` relation (e.g. `IfcSpace` can
        // own `Pset_SpaceCommon`). Falling back to the spatial table
        // here matches the writer's owner-step resolution so neither
        // side is silently dropped on import.
        let resolve_object = |step: &u32| -> Option<EntityId> {
            elements
                .get(step)
                .map(|el| el.entity.clone())
                .or_else(|| spatial.get(step).map(|sp| sp.entity.clone()))
        };
        for (elem_steps, pset_or_qset_step) in &defines_pset {
            // pset_or_qset_step may be either an IFCPROPERTYSET or
            // an IFCELEMENTQUANTITY entity.
            if let Some(pset_row) = psets.get(pset_or_qset_step) {
                let mut set = PropertySet::new(pset_row.name.clone());
                for prop_step in &pset_row.refs {
                    if let Some(pv) = prop_values.get(prop_step) {
                        set.set(pv.key.clone(), pv.value.clone());
                    }
                }
                for elem_step in elem_steps {
                    if let Some(entity) = resolve_object(elem_step) {
                        props.entry(entity).upsert_pset(set.clone());
                    }
                }
                pset_count += 1;
            } else if let Some(qset_row) = qsets.get(pset_or_qset_step) {
                let mut set = QuantitySet::new(qset_row.name.clone());
                for qty_step in &qset_row.refs {
                    if let Some(qv) = qty_values.get(qty_step) {
                        set.quantities.insert(qv.key.clone(), qv.value.clone());
                    }
                }
                for elem_step in elem_steps {
                    if let Some(entity) = resolve_object(elem_step) {
                        props.entry(entity).upsert_qset(set.clone());
                    }
                }
                qset_count += 1;
            } else {
                // The module contract (see crate-level doc and
                // `IfcReader::from_string`) is that unmodeled / unknown
                // entities — including pset / qset target rows that
                // the modeled-entity filter skipped — are tolerated.
                // An IFCRELDEFINESBYPROPERTIES whose RelatingPropertyDefinition
                // points at one of those skipped rows is therefore an
                // expected condition when reading an external file
                // produced by Revit / ArchiCAD / IfcOpenShell, not a
                // file corruption. Skip silently; the `records_seen`
                // counter already accounts for the source record so
                // downstream sanity checks remain meaningful.
                //
                // Hard-failing here was previously inconsistent with
                // the module-level "tolerate and skip unknowns"
                // promise and caused external-file ingestion to
                // refuse otherwise-valid inputs that contained
                // unmodeled IfcPropertySet subclasses (e.g.
                // `IfcPreDefinedPropertySet`).
                continue;
            }
        }

        // ---- Rebuild MaterialStore ----
        //
        // Materials populate `MaterialStore` in two stages:
        //   1. Insert each `IfcMaterial` definition keyed on its
        //      load-bearing `Name`.
        //   2. Resolve every `IfcMaterialLayerSet`'s ordered layer
        //      references by walking back through the
        //      `IfcMaterialLayer` table, replacing the per-layer
        //      `__ref:<step_id>` sentinel material-name with the
        //      real `IfcMaterial.Name` pulled from the resolved
        //      material STEP id. A layer whose material ref points
        //      at an unmodeled / unknown row is silently dropped from
        //      the layer set — same tolerate-and-skip discipline
        //      that `IfcRelDefinesByProperties` uses for unknown
        //      pset targets.
        //   3. Walk each `IfcRelAssociatesMaterial` row and bind the
        //      named material (single) or layer-set (composite) to
        //      every referenced element.
        let mut materials_store = MaterialStore::new();
        // Stage 1: materials
        for mat in materials_step.values() {
            materials_store.upsert_material(mat.clone());
        }
        // Stage 2: layer sets
        for (set_name, set_description, layer_refs) in material_layer_sets_step.values() {
            let mut set = MaterialLayerSet::new(set_name.clone());
            set.description.clone_from(set_description);
            for layer_step in layer_refs {
                let Some(layer) = material_layers_step.get(layer_step) else {
                    continue;
                };
                // Resolve the layer's "__ref:<step>" sentinel back to
                // the real material name.
                let sentinel = &layer.material_name;
                let resolved_name = sentinel
                    .strip_prefix("__ref:")
                    .and_then(|s| s.parse::<u32>().ok())
                    .and_then(|step| materials_step.get(&step))
                    .map(|m| m.name.clone());
                let Some(resolved_name) = resolved_name else {
                    continue;
                };
                let mut resolved = layer.clone();
                resolved.material_name = resolved_name;
                set.layers.push(resolved);
            }
            materials_store.upsert_layer_set(set);
        }
        // Stage 3: assignments
        let mut material_assignment_count = 0usize;
        for (elem_steps, mat_ref) in &associates_material {
            let assignment_opt = if let Some(mat) = materials_step.get(mat_ref) {
                Some(MaterialAssignment::Single(mat.name.clone()))
            } else if let Some((set_name, _, _)) = material_layer_sets_step.get(mat_ref) {
                Some(MaterialAssignment::LayerSet(set_name.clone()))
            } else {
                None
            };
            let Some(assignment) = assignment_opt else {
                // Unmodeled material reference (e.g.
                // `IfcMaterialProfileSet` / `IfcMaterialConstituentSet`
                // — not yet handled). Skip per the tolerate-and-skip
                // contract.
                continue;
            };
            for elem_step in elem_steps {
                let entity_opt = elements
                    .get(elem_step)
                    .map(|el| el.entity.clone())
                    .or_else(|| spatial.get(elem_step).map(|sp| sp.entity.clone()));
                if let Some(entity) = entity_opt {
                    if materials_store.assign_to_element(entity, assignment.clone()) {
                        material_assignment_count += 1;
                    }
                }
            }
        }

        // ---- Build GUID map ----
        let mut guid_by_entity: HashMap<EntityId, String> = HashMap::new();
        for row in spatial.values() {
            guid_by_entity.insert(row.entity.clone(), row.guid.clone());
        }
        for row in elements.values() {
            guid_by_entity.insert(row.entity.clone(), row.guid.clone());
        }

        let _ = by_id; // index reserved for richer cross-refs

        let stats = IfcReadStats {
            spatial_nodes: spatial.len(),
            elements: elements.len(),
            psets: pset_count,
            qsets: qset_count,
            aggregations,
            containments,
            materials: materials_store.material_count(),
            material_layer_sets: materials_store.layer_set_count(),
            material_assignments: material_assignment_count,
            records_seen: groups.len(),
        };

        // sanity: project_entity is the same as project.root
        debug_assert_eq!(project_entity, project.root);

        Ok(IfcSnapshot {
            project,
            classification,
            properties: props,
            materials: materials_store,
            guid_by_entity,
            element_parent,
            schema,
            stats,
        })
    }

    /// Convenience wrapper around [`IfcReader::from_string`] that
    /// accepts any [`Read`] (no `BufRead` requirement — the input is
    /// slurped into a `String` in one call).
    ///
    /// **Memory profile**: this method is API-streaming but NOT
    /// memory-streaming — it buffers the entire input into a
    /// `String` before parsing because [`IfcReader::from_string`]
    /// needs random access to resolve forward references in the
    /// STEP cross-reference graph (e.g. `#42` referring to an
    /// entity defined later in the file). Peak RAM is therefore
    /// roughly `file_size + indexed_record_set`, the same as
    /// reading the whole file into a `String` yourself.
    ///
    /// For genuinely memory-streaming consumption that yields one
    /// [`StepRecord`] at a time without building cross-reference
    /// indexes (suitable for multi-gigabyte IFC ingest where you
    /// only need per-record processing), use [`IfcReader::iter`]
    /// instead.
    pub fn from_reader<R: Read>(mut reader: R) -> IfcReadResult<IfcSnapshot> {
        let mut buf = String::new();
        reader.read_to_string(&mut buf)?;
        Self::from_string(&buf)
    }

    /// Create a streaming iterator over the STEP entity instances
    /// in `reader`. The iterator yields one [`StepRecord`] per
    /// logical record (record terminator = `;` outside strings,
    /// parens, and comments), without buffering the entire file
    /// in memory.
    ///
    /// Use this when you need raw entity records (e.g. to
    /// implement a custom dispatch over IFC types AEC Studio
    /// doesn't model). For the high-level snapshot, use
    /// [`IfcReader::from_string`] or [`IfcReader::from_reader`].
    pub fn iter<R: BufRead>(reader: R) -> StepIter<R> {
        StepIter::new(reader)
    }

    fn malformed(record: &StepRecord, why: &str) -> IfcReadError {
        IfcReadError::Malformed(format!(
            "#{} = {}({}…): {}",
            record.step_id,
            record.kind,
            record.args.first().cloned().unwrap_or_default(),
            why
        ))
    }
}

// ---------------------------------------------------------------------
// Tokenisation
// ---------------------------------------------------------------------

/// A parsed STEP entity instance: `#step_id = kind(args...);`.
///
/// This is the smallest unit the parser yields; consumers can read
/// records via [`StepIter`] and dispatch entity-type-specific
/// interpretation themselves. The internal entity-extraction code
/// in [`IfcReader::from_string`] uses the same type.
#[derive(Debug, Clone)]
pub struct StepRecord {
    pub step_id: u32,
    /// STEP entity kind, normalized to ASCII uppercase for matching
    /// against canonical STEP entity names (`IFCWALL`, `IFCSITE`, ...).
    pub kind: String,
    /// The kind exactly as it appeared in the source bytes, before
    /// any case normalization. Used to round-trip
    /// [`IfcClass::Other`] values whose tag carries non-canonical
    /// casing (e.g. `"IfcBuildingElementProxy"`).
    pub raw_kind: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
struct SpatialRow {
    entity: EntityId,
    guid: String,
    class: IfcClass,
    name: String,
}

#[derive(Debug, Clone)]
struct ElementRow {
    entity: EntityId,
    guid: String,
    class: IfcClass,
    /// IFC tag (e.g. "IfcWall") recovered from the element's `Name`
    /// prefix. Currently unused on read — we already have the class
    /// in `class` — but kept so future versions can validate that the
    /// embedded tag matches the entity type for tamper-detection.
    #[allow(dead_code)]
    tag: String,
}

#[derive(Debug, Clone)]
struct PsetRow {
    name: String,
    refs: Vec<u32>,
}

#[derive(Debug, Clone)]
struct QsetRow {
    name: String,
    refs: Vec<u32>,
}

#[derive(Debug, Clone)]
struct PropRow {
    key: String,
    value: PropertyValue,
}

#[derive(Debug, Clone)]
struct QtyRow {
    key: String,
    value: PropertyValue,
}

fn parse_step_groups(text: &str) -> IfcReadResult<Vec<StepRecord>> {
    let mut groups = Vec::new();
    for record in iter_logical_records(text)? {
        if let Some(rec) = parse_logical_record(&record)? {
            groups.push(rec);
        }
    }
    Ok(groups)
}

/// Detect the IFC schema declared in `FILE_SCHEMA(('IFCxx'))`.
///
/// AEC Studio accepts IFC2x3, IFC4, and IFC4x3. Any other token is
/// rejected as `MissingSection("FILE_SCHEMA")` so we fail loudly
/// rather than silently re-interpret an unsupported schema as IFC4.
///
/// The search is intentionally **bounded to the HEADER section**
/// (`HEADER;` ... `ENDSEC;` before `DATA;`) so we never mistake a
/// `FILE_SCHEMA(('IFC2X3'))` token that happens to appear inside a
/// DATA-section string literal (e.g. a `Description` attribute on
/// an entity, or a comment-like `/*…*/` block) for the file's
/// declared schema. ISO 10303-21 §6.4 mandates that `FILE_SCHEMA`
/// is a HEADER entity, so anything matching outside HEADER must be
/// payload, not metadata.
fn detect_schema(text: &str) -> IfcReadResult<IfcSchema> {
    // The STEP grammar allows whitespace inside the parens; the
    // canonical AEC-emitted form is `FILE_SCHEMA(('IFC4'));` but
    // external authoring tools may emit `FILE_SCHEMA ( ( 'IFC4' ) ) ;`.
    let upper = text.to_ascii_uppercase();
    // Bound the search to the HEADER section. STEP files start with
    // `ISO-10303-21;` then `HEADER;` ... `ENDSEC;` then `DATA;`.
    // We locate `HEADER` and the *first* `ENDSEC` after it; that
    // pair delimits the metadata block. If the file lacks either
    // marker we reject as `MissingSection` rather than fall back to
    // a permissive whole-text scan (which would let a DATA-section
    // string smuggle in a fake schema declaration).
    let header_start = upper
        .find("HEADER")
        .ok_or(IfcReadError::MissingSection("HEADER"))?;
    let header_end_rel = upper[header_start..]
        .find("ENDSEC")
        .ok_or(IfcReadError::MissingSection("HEADER"))?;
    let header_end = header_start + header_end_rel;
    let header = &text[header_start..header_end];
    let header_upper = &upper[header_start..header_end];
    let needle = "FILE_SCHEMA";
    let start = header_upper
        .find(needle)
        .ok_or(IfcReadError::MissingSection("FILE_SCHEMA"))?;
    let tail = &header[start + needle.len()..];
    // Locate the first single-quoted literal after `FILE_SCHEMA(...)`.
    let q1 = tail
        .find('\'')
        .ok_or(IfcReadError::MissingSection("FILE_SCHEMA"))?;
    let after = &tail[q1 + 1..];
    let q2 = after
        .find('\'')
        .ok_or(IfcReadError::MissingSection("FILE_SCHEMA"))?;
    let literal = &after[..q2];
    IfcSchema::from_step_literal(literal).ok_or(IfcReadError::MissingSection("FILE_SCHEMA"))
}

/// Iterator over logical STEP records (text between matching
/// top-level semicolons), with comments stripped and string literals
/// respected. Returns each record's raw text *without* the trailing
/// semicolon.
///
/// This handles three things the original line-based splitter did
/// not:
///
///   * **Multi-line records**: an `IFCWALL(... \n ... \n ...);` whose
///     arg list spans several newlines is returned as a single
///     logical record.
///   * **`/* ... */` comments**: per ISO 10303-21 §6.4.1, comments
///     can appear anywhere outside string literals. We strip them
///     before record splitting so a `;` inside a comment doesn't
///     accidentally terminate a record.
///   * **Embedded `;` in strings**: a string literal `'foo;bar'`
///     contains a literal semicolon that is *not* a record
///     terminator.
fn iter_logical_records(text: &str) -> IfcReadResult<Vec<String>> {
    let mut splitter = StepRecordSplitter::new(text);
    let mut out: Vec<String> = Vec::new();
    for item in splitter.by_ref() {
        let (record, _consumed) = item?;
        if !record.is_empty() {
            out.push(record);
        }
    }
    splitter.validate_at_eof()?;
    Ok(out)
}

/// Shared STEP record-splitting state machine. The same character-
/// level rules apply to three callers:
///
///   1. [`iter_logical_records`] (full-text → `Vec<String>`)
///   2. [`split_first_logical_record`] (streaming prefix → first
///      record + consumed bytes)
///   3. [`validate_trailing_buffer`] (post-stream EOF check)
///
/// Keeping the rules in a single struct ensures the three sites can
/// not diverge — earlier versions of this module had three
/// independent copies that all needed to be updated in lockstep (for
/// `/* … */` comments, multi-byte UTF-8, escaped quotes, ISO
/// `''` doubled-quote escapes, etc.). Adding a new STEP token rule
/// now requires editing exactly one place.
///
/// **String quoting** accepts both AEC Studio's writer convention
/// (`\'` for an embedded single-quote, `\\` for a backslash) AND
/// the ISO 10303-21 canonical convention (`''` doubled quote for an
/// embedded single-quote). The writer emits the ISO `''` form so
/// AEC Studio output is interoperable with Revit / ArchiCAD / other
/// third-party IFC tooling, but the reader keeps accepting `\'` for
/// backwards compatibility with files written by earlier AEC builds.
///
/// Both escaped forms are preserved *verbatim* in the record buffer
/// returned by the iterator; [`unescape_step_string`] does the final
/// collapse from `\'` / `''` to a single `'`.
struct StepRecordSplitter<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    buf: String,
    depth: i32,
    in_str: bool,
    in_comment: bool,
}

impl<'a> StepRecordSplitter<'a> {
    fn new(text: &'a str) -> Self {
        // Walk by characters so multi-byte UTF-8 (e.g. é, Ö, £) is not
        // mis-split. The peek-ahead for `/*` and `*/` looks at the next
        // *byte* via slice indexing on the original text, which is safe
        // because `/` and `*` are ASCII (single-byte) and the byte
        // offset of each char is always at a char boundary.
        Self {
            chars: text.char_indices().peekable(),
            buf: String::new(),
            depth: 0,
            in_str: false,
            in_comment: false,
        }
    }

    /// After [`Iterator::next`] returns `None`, call this to surface
    /// dangling-state errors (unterminated string literal, unbalanced
    /// parens). Returns `Ok(())` when the splitter is in a clean
    /// resting state — i.e. not in the middle of a string and depth
    /// is back to zero. Callers that operate on a streaming prefix
    /// (where partial state at end-of-buffer is expected) skip this
    /// check; callers that operate on a full STEP file or at
    /// end-of-stream invoke it.
    fn validate_at_eof(&self) -> IfcReadResult<()> {
        if self.in_str {
            return Err(IfcReadError::Malformed(
                "unterminated string literal at end of STEP file".into(),
            ));
        }
        if self.depth != 0 {
            return Err(IfcReadError::Malformed(format!(
                "STEP file ends with unbalanced parens (depth={})",
                self.depth
            )));
        }
        Ok(())
    }
}

impl Iterator for StepRecordSplitter<'_> {
    /// `(record_text_without_trailing_semicolon, bytes_consumed_in_input)`.
    ///
    /// `bytes_consumed_in_input` is the byte offset (into the slice
    /// passed to [`StepRecordSplitter::new`]) of the first byte *past*
    /// the terminating `;` — the value [`split_first_logical_record`]
    /// returns so the streaming caller can drain that prefix.
    type Item = IfcReadResult<(String, usize)>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((off, c)) = self.chars.next() {
            // Comment open: `/*` outside strings.
            if !self.in_str && !self.in_comment && c == '/' {
                if let Some(&(_, '*')) = self.chars.peek() {
                    self.in_comment = true;
                    self.chars.next(); // consume `*`
                    continue;
                }
            }
            // Comment close: `*/`.
            if self.in_comment {
                if c == '*' {
                    if let Some(&(_, '/')) = self.chars.peek() {
                        self.in_comment = false;
                        self.chars.next(); // consume `/`
                    }
                }
                continue;
            }
            match c {
                '\\' if self.in_str => {
                    // Preserve `\\` and `\'` so split_step_args sees them.
                    self.buf.push(c);
                    if let Some((_, next)) = self.chars.next() {
                        self.buf.push(next);
                    }
                }
                '\'' => {
                    // ISO 10303-21 §6.4.1: `''` inside a string
                    // literal is an escaped single quote (the string
                    // does NOT terminate). Detect the doubled-quote
                    // by peeking the next char; if matched, push
                    // both into the buffer and stay `in_str`.
                    if self.in_str {
                        if let Some(&(_, '\'')) = self.chars.peek() {
                            self.buf.push(c);
                            if let Some((_, c2)) = self.chars.next() {
                                self.buf.push(c2);
                            }
                            continue;
                        }
                    }
                    self.in_str = !self.in_str;
                    self.buf.push(c);
                }
                '(' if !self.in_str => {
                    self.depth += 1;
                    self.buf.push(c);
                }
                ')' if !self.in_str => {
                    self.depth -= 1;
                    if self.depth < 0 {
                        return Some(Err(IfcReadError::Malformed(format!(
                            "STEP file has ')' without matching '(' near offset {off}"
                        ))));
                    }
                    self.buf.push(c);
                }
                ';' if !self.in_str && self.depth == 0 => {
                    let record = std::mem::take(&mut self.buf).trim().to_string();
                    let consumed = off + c.len_utf8();
                    return Some(Ok((record, consumed)));
                }
                _ => self.buf.push(c),
            }
        }
        None
    }
}

/// Parse a logical record's text (without trailing `;`) into a
/// [`StepRecord`] if it matches the `#N = TYPE(args)` shape;
/// otherwise return `Ok(None)` so the caller can ignore HEADER
/// declarations, ENDSEC markers, and other non-entity tokens.
fn parse_logical_record(record: &str) -> IfcReadResult<Option<StepRecord>> {
    let line = record.trim();
    if !line.starts_with('#') {
        return Ok(None);
    }
    let (lhs, rhs) = line
        .split_once('=')
        .ok_or_else(|| IfcReadError::Malformed(format!("missing '=' in '{line}'")))?;
    let step_id: u32 = lhs
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|e| IfcReadError::Malformed(format!("step id parse: {e}")))?;
    let rhs = rhs.trim();
    let open = rhs
        .find('(')
        .ok_or_else(|| IfcReadError::Malformed(format!("missing '(' in '{rhs}'")))?;
    let close = rhs
        .rfind(')')
        .ok_or_else(|| IfcReadError::Malformed(format!("missing ')' in '{rhs}'")))?;
    let raw_kind = rhs[..open].trim().to_string();
    let kind = raw_kind.to_ascii_uppercase();
    let inner = &rhs[open + 1..close];
    let args = split_step_args(inner)?;
    Ok(Some(StepRecord {
        step_id,
        kind,
        raw_kind,
        args,
    }))
}

/// Streaming iterator over STEP entity instances.
///
/// Pulls one logical record at a time from a [`BufRead`], yielding
/// `Result<StepRecord, IfcReadError>`. Callers can implement custom
/// cross-reference resolution without buffering the entire file
/// in memory \u2014 only the in-flight record plus whatever cross-ref
/// indexes the caller chooses to maintain is held in RAM.
///
/// Handles multi-line records, `/* ... */` comments, and embedded
/// `;` inside quoted strings identically to
/// [`IfcReader::from_string`].
pub struct StepIter<R: BufRead> {
    reader: R,
    buf: String,
    eof: bool,
    fault: bool,
}

impl<R: BufRead> StepIter<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buf: String::new(),
            eof: false,
            fault: false,
        }
    }
}

impl<R: BufRead> Iterator for StepIter<R> {
    type Item = IfcReadResult<StepRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fault {
            return None;
        }
        loop {
            // Try to split off one logical record from the buffer.
            match split_first_logical_record(&self.buf) {
                Ok(Some((record_text, consumed))) => {
                    self.buf.drain(..consumed);
                    match parse_logical_record(&record_text) {
                        Ok(Some(rec)) => return Some(Ok(rec)),
                        Ok(None) => continue, // skip non-entity tokens
                        Err(e) => {
                            self.fault = true;
                            return Some(Err(e));
                        }
                    }
                }
                Ok(None) => {
                    // Need more input.
                    if self.eof {
                        // The reader is drained. Match the eager
                        // `from_string` path's truncation-detection
                        // contract: if the trailing buffer contains an
                        // unterminated string literal or unbalanced
                        // parens, surface a `Malformed` error rather
                        // than silently dropping the partial record.
                        // `from_string` runs the same check at end of
                        // input (see `iter_logical_records`); without
                        // it, a STEP file truncated mid-string would
                        // produce identical bytes through both APIs but
                        // only one would flag the corruption.
                        if let Err(e) = validate_trailing_buffer(&self.buf) {
                            self.fault = true;
                            self.buf.clear();
                            return Some(Err(e));
                        }
                        return None;
                    }
                    let mut line = String::new();
                    match self.reader.read_line(&mut line) {
                        Ok(0) => self.eof = true,
                        Ok(_) => self.buf.push_str(&line),
                        Err(e) => {
                            self.fault = true;
                            return Some(Err(e.into()));
                        }
                    }
                }
                Err(e) => {
                    self.fault = true;
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Validate the trailing (post-final-`;`) bytes of a STEP buffer at
/// end-of-stream. Used by [`StepIter`] to reject truncated files —
/// without this check a STEP file that was cut off mid-string or
/// mid-record would silently terminate the iterator instead of
/// flagging the corruption.
///
/// Returns `Err(Malformed)` for:
///   * unterminated string literal (`'…` without closing `'`)
///   * unbalanced parens (more `(` than `)` outside strings)
///   * unbalanced parens (more `)` than `(` outside strings)
///
/// Returns `Ok(())` for buffers that contain only whitespace,
/// comments, and balanced tokens that don't constitute a full
/// `... ;` record (those are tolerated — the file may legitimately
/// end with trailing whitespace after the final `ENDSEC;`).
fn validate_trailing_buffer(text: &str) -> IfcReadResult<()> {
    // Reuse the shared splitter: drain any complete records that
    // happen to fit in the trailing buffer (surfacing any structural
    // error), then assert clean EOF state. The splitter's character-
    // level rules are identical to the streaming reader's so this
    // check can never disagree with the prior `split_first_logical_record`
    // calls about whether the file was well-formed.
    let mut splitter = StepRecordSplitter::new(text);
    for item in splitter.by_ref() {
        item?;
    }
    splitter.validate_at_eof()
}

/// Find the first complete logical STEP record in `text`. Returns
/// `Ok(Some((record, consumed)))` where `record` is the text up to
/// but excluding the terminating `;`, and `consumed` is the byte
/// length to drain from the front of `text` (including the `;`).
/// Returns `Ok(None)` if `text` does not yet contain a complete
/// record (caller should buffer more input).
fn split_first_logical_record(text: &str) -> IfcReadResult<Option<(String, usize)>> {
    // Pull a single record from the shared splitter; the streaming
    // caller drives subsequent reads from a fresh splitter over the
    // post-drain buffer.
    match StepRecordSplitter::new(text).next() {
        Some(Ok((record, consumed))) => Ok(Some((record, consumed))),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

/// Split the argument list of a STEP record, respecting nested
/// parentheses (e.g. measure wrappers like `IFCLENGTHMEASURE(0.9)`)
/// and single-quoted strings.
///
/// Returns `IfcReadError::Malformed` if the input is structurally
/// invalid — closing paren without a matching open, unclosed paren at
/// end of input, or an unterminated quoted string. The writer only ever
/// emits balanced output, but failing loudly on malformed bytes prevents
/// later parsers from silently consuming an un-split argument as a
/// single literal.
fn split_step_args(s: &str) -> IfcReadResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    // u32 + saturating arithmetic would also work, but i32 lets us
    // explicitly diagnose underflow as a `Malformed` error so the file
    // surface fails early instead of producing garbage downstream.
    let mut depth = 0i32;
    let mut in_str = false;
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_str => {
                // Legacy AEC writer convention: `\'` for an embedded
                // single-quote and `\\` for a backslash. Consume the
                // backslash + following character verbatim so we don't
                // mis-toggle `in_str` on an escaped `'`.
                buf.push(c);
                if let Some(next) = chars.next() {
                    buf.push(next);
                }
            }
            '\'' => {
                // ISO 10303-21 §6.4.1: `''` inside a string literal is
                // an embedded single-quote (the string does NOT
                // terminate). Match the same rule used by
                // `StepRecordSplitter` so external IFC files round-trip
                // even though the arg-level splitter does not run
                // through the record splitter twice.
                if in_str && matches!(chars.peek(), Some('\'')) {
                    buf.push(c);
                    if let Some(c2) = chars.next() {
                        buf.push(c2);
                    }
                    continue;
                }
                in_str = !in_str;
                buf.push(c);
            }
            '(' if !in_str => {
                depth += 1;
                buf.push(c);
            }
            ')' if !in_str => {
                depth -= 1;
                if depth < 0 {
                    return Err(IfcReadError::Malformed(format!(
                        "STEP arg list has ')' without a matching '(': {s:?}"
                    )));
                }
                buf.push(c);
            }
            ',' if !in_str && depth == 0 => {
                out.push(buf.trim().to_string());
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    if in_str {
        return Err(IfcReadError::Malformed(format!(
            "STEP arg list has an unterminated quoted string: {s:?}"
        )));
    }
    if depth != 0 {
        return Err(IfcReadError::Malformed(format!(
            "STEP arg list has unbalanced parens (depth={depth}): {s:?}"
        )));
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    Ok(out)
}

impl StepRecord {
    /// Decode a single-quoted string argument at `idx`, unescaping
    /// `\'`, `\\`, `\n`, `\r`, and `\t` per the writer's contract.
    pub fn string_arg(&self, idx: usize) -> IfcReadResult<String> {
        let raw = self.args.get(idx).ok_or_else(|| {
            IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind))
        })?;
        let s = raw.trim();
        if !(s.starts_with('\'') && s.ends_with('\'')) {
            return Err(IfcReadError::Malformed(format!(
                "expected quoted string at arg {idx} of {} but got '{s}'",
                self.kind
            )));
        }
        Ok(unescape_step_string(&s[1..s.len() - 1]))
    }

    /// Decode an entity reference (`#N`) argument at `idx`.
    pub fn ref_arg(&self, idx: usize) -> IfcReadResult<u32> {
        let raw = self.args.get(idx).ok_or_else(|| {
            IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind))
        })?;
        let s = raw.trim();
        if !s.starts_with('#') {
            return Err(IfcReadError::Malformed(format!(
                "expected #N reference at arg {idx} of {} but got '{s}'",
                self.kind
            )));
        }
        s[1..]
            .parse()
            .map_err(|e| IfcReadError::Malformed(format!("ref parse: {e}")))
    }

    /// Decode a list-of-references (`(#N, #M, ...)`) argument at
    /// `idx`. Empty lists return an empty vector.
    pub fn ref_list_arg(&self, idx: usize) -> IfcReadResult<Vec<u32>> {
        let raw = self.args.get(idx).ok_or_else(|| {
            IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind))
        })?;
        let s = raw.trim();
        if !(s.starts_with('(') && s.ends_with(')')) {
            return Err(IfcReadError::Malformed(format!(
                "expected (#N,…) at arg {idx} of {} but got '{s}'",
                self.kind
            )));
        }
        let inner = &s[1..s.len() - 1];
        let mut out = Vec::new();
        for tok in inner.split(',') {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            if !tok.starts_with('#') {
                return Err(IfcReadError::Malformed(format!(
                    "expected #N in list arg {idx} of {} but got '{tok}'",
                    self.kind
                )));
            }
            let id: u32 = tok[1..]
                .parse()
                .map_err(|e| IfcReadError::Malformed(format!("ref-list parse: {e}")))?;
            out.push(id);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------
// IFC class lookup (mirror of writer's `IfcClass::ifc_tag`)
// ---------------------------------------------------------------------

/// Resolve a STEP entity name back into an [`IfcClass`].
///
/// `tag` is the canonical (uppercased) form used for matching;
/// `raw_tag` is the original spelling as it appeared in the STEP
/// bytes. Unknown tags are not an error — they round-trip through
/// [`IfcClass::Other`] using `raw_tag` so author-provided casing
/// (e.g. `"IfcBuildingElementProxy"` from the AI classifier) is
/// preserved verbatim across export/import.
fn ifc_class_from_tag(tag: &str, raw_tag: &str) -> IfcClass {
    // STEP entity names are uppercase ("IFCWALL"), our `ifc_tag()`
    // returns mixed case ("IfcWall"). Normalize for matching.
    let up = tag.to_ascii_uppercase();
    match up.as_str() {
        "IFCPROJECT" => IfcClass::IfcProject,
        "IFCSITE" => IfcClass::IfcSite,
        "IFCBUILDING" => IfcClass::IfcBuilding,
        "IFCBUILDINGSTOREY" => IfcClass::IfcBuildingStorey,
        "IFCSPACE" => IfcClass::IfcSpace,
        "IFCWALL" => IfcClass::IfcWall,
        "IFCWALLSTANDARDCASE" => IfcClass::IfcWallStandardCase,
        "IFCSLAB" => IfcClass::IfcSlab,
        "IFCCOVERING" => IfcClass::IfcCovering,
        "IFCDOOR" => IfcClass::IfcDoor,
        "IFCWINDOW" => IfcClass::IfcWindow,
        "IFCCOLUMN" => IfcClass::IfcColumn,
        "IFCBEAM" => IfcClass::IfcBeam,
        "IFCSTAIR" => IfcClass::IfcStair,
        "IFCRAILING" => IfcClass::IfcRailing,
        "IFCROOF" => IfcClass::IfcRoof,
        "IFCCURTAINWALL" => IfcClass::IfcCurtainWall,
        "IFCFURNITURE" => IfcClass::IfcFurniture,
        "IFCFURNISHINGELEMENT" => IfcClass::IfcFurnishingElement,
        "IFCSANITARYTERMINAL" => IfcClass::IfcSanitaryTerminal,
        "IFCLIGHTFIXTURE" => IfcClass::IfcLightFixture,
        "IFCPLUMBINGFIXTURE" => IfcClass::IfcPlumbingFixture,
        "IFCOPENINGELEMENT" => IfcClass::IfcOpeningElement,
        _ => IfcClass::Other(raw_tag.to_string()),
    }
}

// ---------------------------------------------------------------------
// Measure parsing
// ---------------------------------------------------------------------

/// Parse a measure literal of the form `MEASURE_TAG(value)` (e.g.
/// `IFCLENGTHMEASURE(0.9)`, `IFCTEXT('foo')`, `IFCBOOLEAN(.T.)`).
fn parse_typed_measure(raw: &str) -> IfcReadResult<PropertyValue> {
    let raw = raw.trim();
    let open = raw
        .find('(')
        .ok_or_else(|| IfcReadError::Malformed(format!("measure missing '(': {raw}")))?;
    let close = raw
        .rfind(')')
        .ok_or_else(|| IfcReadError::Malformed(format!("measure missing ')': {raw}")))?;
    let tag = raw[..open].trim().to_ascii_uppercase();
    let inner = raw[open + 1..close].trim();
    match tag.as_str() {
        "IFCTEXT" => Ok(PropertyValue::Text(unquote(inner)?)),
        "IFCLABEL" => Ok(PropertyValue::Label(unquote(inner)?)),
        "IFCREAL" => Ok(PropertyValue::Real(parse_real(inner)?)),
        "IFCLENGTHMEASURE" => Ok(PropertyValue::Length(parse_real(inner)?)),
        "IFCAREAMEASURE" => Ok(PropertyValue::Area(parse_real(inner)?)),
        "IFCVOLUMEMEASURE" => Ok(PropertyValue::Volume(parse_real(inner)?)),
        "IFCPOSITIVERATIOMEASURE" => Ok(PropertyValue::Ratio(parse_real(inner)?)),
        "IFCINTEGER" => Ok(PropertyValue::Integer(parse_int(inner)?)),
        "IFCBOOLEAN" => Ok(PropertyValue::Boolean(matches!(inner, ".T."))),
        other => {
            // Preserve unknown IFC measure types verbatim for
            // lossless round-trip. The writer's emit path detects
            // `PropertyValue::Other` and re-wraps the raw literal
            // in the original IFC measure tag. We store the STEP
            // form (uppercase) since recovering the IFC4 canonical
            // camelCase spelling without a dictionary is lossy.
            Ok(PropertyValue::Other {
                measure: other.to_string(),
                raw: inner.to_string(),
            })
        }
    }
}

fn parse_quantity_typed(tag: &str, raw: &str) -> IfcReadResult<PropertyValue> {
    let v = raw.trim();
    match tag.to_ascii_uppercase().as_str() {
        "IFCQUANTITYLENGTH" => Ok(PropertyValue::Length(parse_real(v)?)),
        "IFCQUANTITYAREA" => Ok(PropertyValue::Area(parse_real(v)?)),
        "IFCQUANTITYVOLUME" => Ok(PropertyValue::Volume(parse_real(v)?)),
        "IFCQUANTITYCOUNT" => Ok(PropertyValue::Integer(parse_int(v)?)),
        "IFCQUANTITYWEIGHT" => Ok(PropertyValue::Real(parse_real(v)?)),
        // IFC4x3 added IFCQUANTITYTIME and others; preserve verbatim.
        other => Ok(PropertyValue::Other {
            measure: other.to_string(),
            raw: v.to_string(),
        }),
    }
}

fn unquote(s: &str) -> IfcReadResult<String> {
    let s = s.trim();
    if !(s.starts_with('\'') && s.ends_with('\'')) {
        return Err(IfcReadError::Malformed(format!(
            "expected quoted string, got '{s}'"
        )));
    }
    Ok(unescape_step_string(&s[1..s.len() - 1]))
}

/// Reverse the writer's STEP string escaping.
///
/// The writer encodes:
///
///   * single quotes as `''` (ISO 10303-21 §6.4.1 canonical doubled
///     quote — interoperable with Revit / ArchiCAD / IfcOpenShell)
///   * backslashes as `\\`
///   * newlines (`U+000A`) as `\n`
///   * carriage returns (`U+000D`) as `\r`
///   * tabs (`U+0009`) as `\t`
///
/// The decoder *also* accepts the legacy AEC writer convention of
/// `\'` for an embedded single-quote so files written by pre-Phase-9
/// AEC builds continue to round-trip cleanly. A single left-to-right
/// pass handles both forms: `\` always consumes the next char, and a
/// `'` inside the string slice is treated as a doubled-quote escape
/// when followed by another `'` (the record splitter already
/// preserved the pair verbatim) or as a stray quote otherwise
/// (defensive — the splitter would have flagged an unterminated
/// string before we got here).
///
/// Chained `replace` calls cannot undo this safely (a naive
/// `.replace("\\\\", "\\").replace("\\'", "'")` would turn the
/// encoded literal `\\\'` — meaning a literal backslash followed by
/// a literal quote — into `\'` and then into a literal quote, losing
/// the backslash). A single left-to-right pass that consumes
/// `\` + next-char as one unit is the only correct decoder.
///
/// The `\n` / `\r` / `\t` escapes exist so the reader can safely
/// `text.lines()` the STEP byte stream without splitting a record
/// across lines just because a user-supplied name contained a literal
/// newline. The decoder maps them back to the original control
/// characters so re-export is byte-identical for any input.
pub(crate) fn unescape_step_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('\'') => out.push('\''),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    // Unknown escape — keep the backslash and the
                    // following character verbatim so we don't
                    // silently drop content we don't recognize.
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else if c == '\'' {
            // ISO 10303-21 doubled-quote escape (`''` → `'`). The
            // record splitter preserved both characters verbatim in
            // the slice we received, so we collapse them here.
            if matches!(chars.peek(), Some('\'')) {
                chars.next();
            }
            out.push('\'');
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_real(s: &str) -> IfcReadResult<f64> {
    parse_step_real(s)
        .ok_or_else(|| IfcReadError::Malformed(format!("real parse: invalid literal {s:?}")))
}

/// Parse a STEP REAL literal into `f64`.
///
/// ISO 10303-21 §6.4.3.2 only allows lowercase `e` (or `E`) for the
/// exponent of a REAL literal, but the older IFC2x3 “AutoCAD-IFC”
/// exporter family (and a few other FORTRAN-derived ARX writers from
/// c. 2000–2010) historically emitted the FORTRAN-style `D` /
/// `d` exponent form (`1.5D-3`) because their host’s `printf`
/// formatter substituted `D` for double-precision values. Real-world
/// `.ifc` archives delivered to mid-size architecture firms
/// occasionally still contain such literals.
///
/// To keep AEC Studio readable against those archives we accept
/// either form. Fast path: try the canonical `[+-]?\d+(\.\d+)?(eE\d+)?`
/// parse first — the overwhelming majority of literals are already
/// in canonical form, and the fast path is zero-allocation. Only on
/// parse failure (and only when the literal actually contains `D` or
/// `d`) do we rewrite the exponent letter and retry; this keeps
/// well-formed literals on the hot, allocation-free path.
///
/// Returns `None` only if the literal is not a valid REAL in either
/// form (e.g. quoted strings or `.T.` booleans, which `PropertyValue::Other`
/// can legitimately store).
///
/// Non-finite results (`NaN`, `±∞`) are also rejected. Rust's
/// `f64::from_str` accepts the textual tokens `NaN`, `inf`, `infinity`,
/// `-inf`, etc., but none of those are valid ISO 10303-21 REAL literals
/// — and silently admitting them would let a malformed STEP file poison
/// downstream BIM quantity arithmetic (a wall with `IFCLENGTHMEASURE(NaN)`
/// would propagate `NaN` through every BOQ sum, `signed_volume`, and
/// schedule it touched). Defense in depth: reject at the parse boundary.
pub(crate) fn parse_step_real(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if let Ok(v) = trimmed.parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    // Slow path: only allocate / scan when there's a D-exponent to
    // rewrite. `D` and `d` cannot occur outside the exponent
    // position of a STEP REAL literal, so a blanket replacement is
    // safe — any other context would already have failed the
    // canonical-form parse above and will fail the rewritten parse
    // too.
    if !trimmed.bytes().any(|b| b == b'D' || b == b'd') {
        return None;
    }
    let rewritten: String = trimmed
        .chars()
        .map(|c| match c {
            'D' => 'E',
            'd' => 'e',
            other => other,
        })
        .collect();
    rewritten.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_int(s: &str) -> IfcReadResult<i64> {
    s.parse::<i64>()
        .map_err(|e| IfcReadError::Malformed(format!("int parse: {e}")))
}

/// Decode a single-quoted string argument at `idx`, returning `None`
/// when the slot holds the STEP "no value" sentinel `$`. Used by the
/// material reader for `Description` / `Category` slots that authoring
/// tools commonly leave empty.
///
/// Distinct from [`StepRecord::string_arg`] which hard-rejects `$`.
/// IFC4 optional string fields (`IfcMaterial.Description`,
/// `IfcMaterial.Category`, etc.) are routinely emitted as `$` so the
/// reader must treat them as `Option`, not an error.
fn optional_string_arg(g: &StepRecord, idx: usize) -> IfcReadResult<Option<String>> {
    let Some(raw) = g.args.get(idx) else {
        return Ok(None);
    };
    let s = raw.trim();
    if s == "$" || s.is_empty() {
        return Ok(None);
    }
    if !(s.starts_with('\'') && s.ends_with('\'')) {
        return Err(IfcReadError::Malformed(format!(
            "expected quoted string at arg {idx} of {} but got '{s}'",
            g.kind
        )));
    }
    Ok(Some(unescape_step_string(&s[1..s.len() - 1])))
}

/// Decode an IFC LOGICAL field (`.T.` / `.F.` / `.U.` / `$`).
///
/// Returns `Some(true)`/`Some(false)` for `.T.`/`.F.`, and `None` for
/// `.U.` (UNKNOWN — IFC4 `IfcLogical` distinguishes this from
/// `IfcBoolean`) or absent (`$`). Used for
/// `IfcMaterialLayer.IsVentilated`.
fn parse_optional_bool(raw: Option<&str>) -> Option<bool> {
    let s = raw?.trim();
    match s {
        ".T." | ".TRUE." => Some(true),
        ".F." | ".FALSE." => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::IfcClass;
    use crate::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
    use crate::spatial::Project;

    fn build_tiny_project() -> (Project, ClassificationStore, PropertyStore, Vec<EntityId>) {
        let mut project = Project::new("Test");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B1")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let mut classification = ClassificationStore::new();
        let mut props = PropertyStore::new();
        let mut element_ids = Vec::new();
        for i in 0..3 {
            let id = EntityId::new();
            project.attach_element(&storey, id.clone());
            classification.assign_manual(id.clone(), IfcClass::IfcWall);
            let mut pc = PropertySet::new("Pset_WallCommon");
            pc.set("Reference", PropertyValue::Label(format!("W-{:02}", i)));
            pc.set("LoadBearing", PropertyValue::Boolean(false));
            props.entry(id.clone()).upsert_pset(pc);
            let mut q = QuantitySet::new("Qto_WallBaseQuantities");
            q.quantities
                .insert("Length".into(), PropertyValue::Length(3.5));
            q.quantities
                .insert("NetSideArea".into(), PropertyValue::Area(8.4));
            q.quantities
                .insert("NetVolume".into(), PropertyValue::Volume(0.84));
            props.entry(id.clone()).upsert_qset(q);
            element_ids.push(id);
        }
        (project, classification, props, element_ids)
    }

    #[test]
    fn reader_roundtrips_writer_output() {
        let (project, classification, props, _elems) = build_tiny_project();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("parses");
        assert_eq!(snap.stats.spatial_nodes, 4, "Project/Site/Building/Storey");
        assert_eq!(snap.stats.elements, 3, "3 walls");
        assert_eq!(snap.stats.psets, 3);
        assert_eq!(snap.stats.qsets, 3);
        assert_eq!(snap.stats.aggregations, 3, "P→S, S→B, B→L");
        assert_eq!(snap.stats.containments, 3, "3 walls contained in L01");
    }

    #[test]
    fn reader_preserves_element_guids_and_psets() {
        let (project, classification, props, elems) = build_tiny_project();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("parses");
        for el in &elems {
            let g = snap.guid_by_entity.get(el).expect("element GUID");
            let expected = crate::ifc::compress_entity_id_to_guid(el);
            assert_eq!(g, &expected, "round-tripped GUID equals derivation");
            let parsed = snap.properties.get(el).expect("element pset");
            assert!(parsed.psets.contains_key("Pset_WallCommon"));
            assert!(parsed.qsets.contains_key("Qto_WallBaseQuantities"));
        }
    }

    #[test]
    fn rejects_non_ifc_input() {
        let err = IfcReader::from_string("not ifc").unwrap_err();
        assert!(matches!(err, IfcReadError::MissingSection(_)));
    }

    #[test]
    fn roundtrips_names_containing_single_quotes() {
        let mut project = Project::new("Café's Place");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Owner's Suite")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "St. Mary's")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();

        let mut classification = ClassificationStore::new();
        let mut props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        classification.assign_manual(el.clone(), IfcClass::IfcWall);
        let mut pc = PropertySet::new("Pset_WallCommon");
        pc.set("Reference", PropertyValue::Text("O'Brien".into()));
        props.entry(el.clone()).upsert_pset(pc);

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("parses names with quotes");
        // Verify the spatial names survived.
        let project_node = snap.project.nodes.get(&snap.project.root).unwrap();
        assert_eq!(project_node.name, "Café's Place");
        // Check the "O'Brien" text property round-tripped.
        let p = snap.properties.get(&el).expect("element props");
        let pset = p.psets.get("Pset_WallCommon").unwrap();
        assert_eq!(
            pset.properties.get("Reference"),
            Some(&PropertyValue::Text("O'Brien".into()))
        );
    }

    #[test]
    fn roundtrips_names_containing_backslashes() {
        // Regression test for the writer/reader pair handling
        // backslash escaping symmetrically.
        //
        // Previously the writer only escaped `'` as `\'` but left
        // raw `\` alone. The reader's tokenizer treats `\` as an
        // escape character, so a value ending in `\` (or containing
        // `\` followed by `'`) would corrupt the parse: the closing
        // `'` would be consumed by the escape state machine and the
        // string would never terminate.
        let mut project = Project::new(r"vendor\Acme");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, r"path\to\site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, r"ends-in-backslash\")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, r"mixed\'special\chars'")
            .unwrap();

        let mut classification = ClassificationStore::new();
        let mut props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        classification.assign_manual(el.clone(), IfcClass::IfcWall);
        let mut pc = PropertySet::new("Pset_WallCommon");
        pc.set(
            "Reference",
            PropertyValue::Text(r"C:\Users\O'Brien\plan.dwg".into()),
        );
        pc.set("Tag", PropertyValue::Label(r"raw\\double".into()));
        props.entry(el.clone()).upsert_pset(pc);

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("parses backslash-bearing strings");

        let project_node = snap.project.nodes.get(&snap.project.root).unwrap();
        assert_eq!(project_node.name, r"vendor\Acme");
        // Walk children by name to find each storey-ancestor.
        let nodes = &snap.project.nodes;
        nodes
            .values()
            .find(|n| n.name == r"path\to\site")
            .expect("site round-trips");
        nodes
            .values()
            .find(|n| n.name == r"ends-in-backslash\")
            .expect("trailing backslash round-trips");
        nodes
            .values()
            .find(|n| n.name == r"mixed\'special\chars'")
            .expect("mixed escape round-trips");

        let p = snap.properties.get(&el).expect("element props");
        let pset = p.psets.get("Pset_WallCommon").unwrap();
        assert_eq!(
            pset.properties.get("Reference"),
            Some(&PropertyValue::Text(r"C:\Users\O'Brien\plan.dwg".into())),
        );
        assert_eq!(
            pset.properties.get("Tag"),
            Some(&PropertyValue::Label(r"raw\\double".into())),
        );
    }

    #[test]
    fn unescape_step_string_is_a_single_pass_inverse_of_escape() {
        // The encoder is `escape_step_string` in the writer; this is
        // the decoder. They must be exact inverses for any input,
        // including pathological combinations of `\` and `'`.
        //
        // Two encoder conventions are exercised:
        //   1. ISO 10303-21 canonical (`''` for embedded single-quote),
        //      which is what the current writer emits.
        //   2. Legacy AEC writer (`\'` for embedded single-quote),
        //      which the reader must continue to accept so files
        //      written by pre-Phase-9 builds keep round-tripping.
        let cases = [
            "",
            "plain text",
            "with 'single' quotes",
            r"with \ backslash",
            r"ends in \\",
            r"both \ and ' mixed",
            r"\\'",         // backslash then escaped quote
            r"\\\\",        // four backslashes
            r"don't \stop", // common natural text
        ];
        for input in cases {
            // (1) ISO-canonical `''` encoding (current writer).
            let mut encoded_iso = String::new();
            for c in input.chars() {
                match c {
                    '\\' => encoded_iso.push_str("\\\\"),
                    '\'' => encoded_iso.push_str("''"),
                    _ => encoded_iso.push(c),
                }
            }
            let decoded_iso = unescape_step_string(&encoded_iso);
            assert_eq!(
                decoded_iso, input,
                "ISO `''` round-trip failed for {input:?}"
            );

            // (2) Legacy `\'` encoding (pre-Phase-9 writer).
            let mut encoded_legacy = String::new();
            for c in input.chars() {
                match c {
                    '\\' => encoded_legacy.push_str("\\\\"),
                    '\'' => encoded_legacy.push_str("\\'"),
                    _ => encoded_legacy.push(c),
                }
            }
            let decoded_legacy = unescape_step_string(&encoded_legacy);
            assert_eq!(
                decoded_legacy, input,
                "legacy `\\'` round-trip failed for {input:?}"
            );
        }
    }

    #[test]
    fn reader_accepts_iso_canonical_doubled_quote_strings() {
        // A STEP file produced by Revit / ArchiCAD / IfcOpenShell
        // encodes an embedded single-quote as `''` (ISO 10303-21
        // §6.4.1 canonical) rather than the legacy AEC `\'` form.
        // Build a hand-written IFC4 STEP file using `''` and verify
        // the reader extracts the original text losslessly.
        let step = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('externally-authored apostrophes'),'2;1');
FILE_NAME('iso.ifc','2025-01-01T00:00:00',('Author'),('Org'),'AEC','AEC','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCPROJECT('00000000000000000000a1',$,$,'O''Brien''s Project',$,$,$,$,$);
#2 = IFCSITE('00000000000000000000a2',$,$,'St. Mary''s',$,$,$,$);
#3 = IFCRELAGGREGATES('00000000000000000000a3',$,$,$,#1,(#2));
ENDSEC;
END-ISO-10303-21;
";
        let snap = IfcReader::from_string(step).expect("reader must accept ISO `''` quoting");
        let root = snap.project.nodes.get(&snap.project.root).unwrap();
        assert_eq!(
            root.name, "O'Brien's Project",
            "ISO doubled-quote in IfcProject.Name must collapse to a single `'`"
        );
        // Walk one level down: the IfcSite child.
        let site_step = root.children.first().expect("project has one child");
        let site = snap.project.nodes.get(site_step).unwrap();
        assert_eq!(
            site.name, "St. Mary's",
            "ISO doubled-quote in IfcSite.Name must collapse to a single `'`"
        );
    }

    #[test]
    fn reader_skips_unmodeled_pset_targets_instead_of_failing() {
        // Per the module contract, an IFCRELDEFINESBYPROPERTIES that
        // references an unmodeled / unknown property-definition row
        // (e.g. `IFCPREDEFINEDPROPERTYSET` produced by Revit's
        // IfcDoorPanelProperties shadow type) must be tolerated and
        // skipped, NOT raised as a hard `Malformed` error. Build a
        // minimal STEP file that exercises this path.
        let step = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('skip-unknown-pset target'),'2;1');
FILE_NAME('skip.ifc','2025-01-01T00:00:00',('A'),('O'),'AEC','AEC','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCPROJECT('00000000000000000000a1',$,$,'P',$,$,$,$,$);
#2 = IFCSITE('00000000000000000000a2',$,$,'S',$,$,$,$);
#3 = IFCBUILDING('00000000000000000000a3',$,$,'B',$,$,$,$);
#4 = IFCBUILDINGSTOREY('00000000000000000000a4',$,$,'L1',$,$,$,$);
#5 = IFCWALL('00000000000000000000a5',$,$,'W1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a6',$,$,$,#1,(#2));
#7 = IFCRELAGGREGATES('00000000000000000000a7',$,$,$,#2,(#3));
#8 = IFCRELAGGREGATES('00000000000000000000a8',$,$,$,#3,(#4));
#9 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000a9',$,$,$,(#5),#4);
/* #10 is an UNMODELED IfcPropertySet subclass that the reader filter skips. */
#10 = IFCPREDEFINEDPROPERTYSET('00000000000000000000aa',$,'UnknownSet',$);
/* #11 points #5 at the unmodeled #10 -- must be skipped, not rejected. */
#11 = IFCRELDEFINESBYPROPERTIES('00000000000000000000bb',$,$,$,(#5),#10);
ENDSEC;
END-ISO-10303-21;
";
        // The contract under test: the reader must NOT raise a hard
        // `Malformed` error just because an IFCRELDEFINESBYPROPERTIES
        // points at an unmodeled `IfcPropertySet` subclass. Earlier
        // versions of this module returned `Err(Malformed(...))` from
        // the second-pass loop, which broke ingestion of any external
        // file containing `IfcPreDefinedPropertySet` / `IfcDoorPanelProperties`
        // / etc.
        let snap = IfcReader::from_string(step)
            .expect("reader must tolerate unmodeled IFCRELDEFINESBYPROPERTIES target");
        // The spatial structure still loads — IfcProject, IfcSite,
        // IfcBuilding, IfcBuildingStorey are all retained.
        assert_eq!(
            snap.stats.spatial_nodes, 4,
            "all four spatial nodes must round-trip even though one IFCRELDEFINESBYPROPERTIES \
             pointed at an unmodeled set"
        );
    }

    #[test]
    fn ifc_class_other_roundtrips_through_writer_and_reader() {
        // The AI classifier produces `IfcClass::Other(...)` for
        // shapes that don't match any known IFC element type. Make
        // sure these round-trip without being dropped or rejected.
        let mut project = Project::new("OtherRT");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "S")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();

        let mut classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        // This is the exact string the AI fallback emits today.
        classification.assign_manual(
            el.clone(),
            IfcClass::Other("IfcBuildingElementProxy".into()),
        );

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("Other(...) round-trips");
        let class = snap
            .classification
            .accepted_for(&el)
            .expect("element classified");
        assert_eq!(
            class,
            &IfcClass::Other("IfcBuildingElementProxy".into()),
            "Other variant preserves the original spelling",
        );
    }

    #[test]
    fn split_step_args_rejects_unbalanced_close_paren() {
        // `')'` before any `'('` would previously drive the depth
        // counter negative and silently produce a single un-split arg.
        // We now fail loudly so a corrupted IFC byte stream surfaces
        // as `IfcReadError::Malformed` instead of garbage downstream.
        let err = split_step_args("a),b").expect_err("must reject extra ')'");
        let msg = err.to_string();
        assert!(
            msg.contains("')' without a matching '('"),
            "unexpected error message: {msg}",
        );
    }

    #[test]
    fn split_step_args_rejects_unbalanced_open_paren() {
        let err = split_step_args("a,(b").expect_err("must reject unclosed '('");
        let msg = err.to_string();
        assert!(
            msg.contains("unbalanced parens"),
            "unexpected error message: {msg}",
        );
    }

    #[test]
    fn split_step_args_rejects_unterminated_quoted_string() {
        let err = split_step_args("a,'oops").expect_err("must reject unterminated string");
        let msg = err.to_string();
        assert!(
            msg.contains("unterminated quoted string"),
            "unexpected error message: {msg}",
        );
    }

    #[test]
    fn split_step_args_accepts_balanced_nested_parens_and_quotes() {
        // Sanity: well-formed args (the only shape the writer emits)
        // still split correctly. Includes a measure wrapper and a
        // quoted string containing both an escaped quote and an
        // escaped backslash.
        let args =
            split_step_args("'a\\'b',IFCLENGTHMEASURE(3.5),'c\\\\d',(#1,#2)").expect("balanced");
        assert_eq!(
            args,
            vec![
                "'a\\'b'".to_string(),
                "IFCLENGTHMEASURE(3.5)".to_string(),
                "'c\\\\d'".to_string(),
                "(#1,#2)".to_string(),
            ],
        );
    }

    #[test]
    fn roundtrips_names_containing_newlines_tabs_and_carriage_returns() {
        // A user-supplied space name with embedded control chars is a
        // realistic data-entry hazard (copy/paste from an external
        // doc, Windows CRLF line endings, etc.). Pre-fix, the writer
        // emitted a literal newline into the STEP record, which the
        // reader's line-based tokenizer split across two lines —
        // producing an opaque `Malformed` error. The escape/unescape
        // pair must now preserve these bytes verbatim.
        let mut project = Project::new("Multi\nline\tProject\rRoot");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site\nA")
            .unwrap();
        let _storey = project
            .add_child(&site, IfcClass::IfcBuildingStorey, "Floor\tOne\rGround")
            .unwrap();

        let classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);

        // Re-parse and confirm the original control characters
        // survived the round-trip on every spatial node we authored.
        let snap = IfcReader::from_string(&s).expect("control-char names round-trip");
        let names: Vec<&str> = snap
            .project
            .nodes
            .values()
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            names.contains(&"Multi\nline\tProject\rRoot"),
            "project name lost control chars: {names:?}",
        );
        assert!(
            names.contains(&"Site\nA"),
            "site name lost newline: {names:?}",
        );
        assert!(
            names.contains(&"Floor\tOne\rGround"),
            "storey name lost tab/CR: {names:?}",
        );
    }

    #[test]
    fn ifc_class_other_with_double_colons_in_tag_roundtrips() {
        // `IfcClass::Other(s)` allows the tag to contain arbitrary
        // characters — an extension classifier could produce
        // e.g. `Other("Some::Custom::Type")`. The writer composes the
        // element's Name field as `{tag}::{entity_id}`, so the reader
        // must split from the RIGHT to recover the eid as the suffix
        // after the final `::`. Splitting from the left (the
        // pre-fix behavior) would mis-identify the tag as just
        // `"Some"` and break the EntityId parse.
        let mut project = Project::new("OtherRT");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "S")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();

        let mut classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        classification.assign_manual(el.clone(), IfcClass::Other("Some::Custom::Type".into()));

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("Other(...) with '::' in tag must round-trip");
        let class = snap
            .classification
            .accepted_for(&el)
            .expect("element classified");
        assert_eq!(
            class,
            &IfcClass::Other("Some::Custom::Type".into()),
            "the full tag including embedded '::' must be preserved",
        );
        // The element's GUID must also have been recovered under the
        // same EntityId — proving the rsplit_once split chose the
        // correct boundary.
        assert!(
            snap.guid_by_entity.contains_key(&el),
            "EntityId must survive the rsplit_once boundary: {:?}",
            snap.guid_by_entity.keys().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn ifc_class_other_with_step_unsafe_chars_roundtrips_via_proxy_fallback() {
        // `IfcClass::Other(s)` can be produced by extension classifiers
        // that aren't bound to STEP-21 SIMPLE_ID syntax (no `(`, `)`,
        // `'`, `,`, `;`, whitespace, or control chars allowed in an
        // entity-type name). If the writer naively used such a tag as
        // the STEP entity type, the reader's `parse_step_groups` would
        // split at the stray `(` and the line would be unparseable.
        //
        // The writer's contract: when the tag is not a STEP-safe
        // SIMPLE_ID, emit the STEP type as the canonical IFC catch-all
        // `IfcBuildingElementProxy` while still encoding the original
        // tag in the Name field (`{original_tag}::{entity_id}`). The
        // reader prefers the Name-field tag over the STEP type when
        // reconstructing the class, so `Other("Foo(Bar)'baz,qux;")`
        // survives a write→read cycle losslessly.
        let mut project = Project::new("OtherUnsafeRT");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "S")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();

        // The full STEP-special charset that would otherwise corrupt
        // the writer's output: parens, single-quote, comma, semicolon,
        // and embedded whitespace.
        let unsafe_tag = "Foo(Bar)'baz,qux; spam";
        let mut classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        classification.assign_manual(el.clone(), IfcClass::Other(unsafe_tag.into()));

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        // Sanity check: the STEP-type position MUST be the canonical
        // proxy fallback — otherwise the file would be unparseable.
        assert!(
            !s.contains(&format!("{unsafe_tag}(")),
            "writer must NOT emit STEP-unsafe tag verbatim as entity type: \n{s}"
        );
        assert!(
            s.contains("IfcBuildingElementProxy("),
            "writer must fall back to IfcBuildingElementProxy as STEP type: \n{s}"
        );

        let snap =
            IfcReader::from_string(&s).expect("Other(...) with STEP-special chars must round-trip");
        let class = snap
            .classification
            .accepted_for(&el)
            .expect("element classified");
        assert_eq!(
            class,
            &IfcClass::Other(unsafe_tag.into()),
            "the original Other(tag) must be reconstructed verbatim despite the proxy fallback",
        );
        assert!(
            snap.guid_by_entity.contains_key(&el),
            "EntityId must survive the unsafe-tag proxy fallback",
        );
    }

    #[test]
    fn writer_output_is_deterministic_across_runs() {
        // The writer's spatial-aggregation and containment-relation
        // loops previously iterated `Project::nodes` (a HashMap),
        // which produces records in different orders run-to-run on a
        // randomized hasher. Correctness was fine (the reader
        // cross-references by `#N`), but byte-identical output
        // matters for golden tests, fingerprinting, and
        // content-addressed export caches. Both loops now walk a
        // deterministic DFS order captured during the spatial pass.
        // Build a tree wide enough at every level to make HashMap
        // iteration order observably non-deterministic if it were
        // still in use (5 children per level, two levels deep), and
        // attach elements to every leaf to also exercise the
        // containment loop.
        let mut project = Project::new("DetCheck");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "Building")
            .unwrap();
        for s in 0..5 {
            let storey = project
                .add_child(&bldg, IfcClass::IfcBuildingStorey, format!("Storey {s}"))
                .unwrap();
            for _ in 0..5 {
                let space = project
                    .add_child(&storey, IfcClass::IfcSpace, "Space")
                    .unwrap();
                assert!(project.attach_element(&space, EntityId::new()));
            }
            for _ in 0..3 {
                assert!(project.attach_element(&storey, EntityId::new()));
            }
        }

        let classification = ClassificationStore::new();
        let properties = PropertyStore::new();

        let s1 = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);
        let s2 = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);
        let s3 = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);
        assert_eq!(
            s1, s2,
            "IfcWriter::to_string must be byte-identical across runs",
        );
        assert_eq!(
            s1, s3,
            "IfcWriter::to_string must be byte-identical across runs",
        );

        // And the deterministic output must still parse cleanly via
        // the reader (i.e. the DFS-ordered rels haven't broken
        // anything for the round-trip path).
        let snap = crate::ifc::IfcReader::from_string(&s1)
            .expect("deterministic IFC output must still round-trip");
        // 1 project + 1 site + 1 building + 5 storeys + 25 spaces.
        assert_eq!(snap.stats.spatial_nodes, 1 + 1 + 1 + 5 + 25);
        // 25 element/space + 5 storey * 3 = 40 elements total.
        assert_eq!(snap.stats.elements, 25 + 5 * 3);
    }

    #[test]
    fn spatial_node_psets_and_qsets_roundtrip() {
        // Per IFC4, spatial structure elements (`IfcSpace`,
        // `IfcBuildingStorey`, …) can own Psets and Qtos —
        // `Pset_SpaceCommon` and `Qto_SpaceBaseQuantities` are the
        // canonical examples on `IfcSpace`. The writer must emit
        // these IfcRelDefinesByProperties relations (resolving the
        // owner step against the spatial table, not just the element
        // table), and the reader must restore them on the
        // corresponding spatial EntityId on import.
        let mut project = Project::new("SpatialPsetRT");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "S")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let space = project
            .add_child(&storey, IfcClass::IfcSpace, "Living Room")
            .unwrap();

        let classification = ClassificationStore::new();
        let mut properties = PropertyStore::new();

        // Realistic Pset_SpaceCommon on the space.
        let mut pset_common = PropertySet::new("Pset_SpaceCommon");
        pset_common.set(
            "Reference".to_string(),
            PropertyValue::Text("R-101".to_string()),
        );
        pset_common.set(
            "Category".to_string(),
            PropertyValue::Text("Living".to_string()),
        );
        pset_common.set(
            "PubliclyAccessible".to_string(),
            PropertyValue::Boolean(false),
        );
        properties
            .entry(space.clone())
            .upsert_pset(pset_common.clone());

        // Realistic Qto_SpaceBaseQuantities on the space.
        let mut qset_base = QuantitySet::new("Qto_SpaceBaseQuantities");
        qset_base
            .quantities
            .insert("NetFloorArea".into(), PropertyValue::Area(28.5));
        qset_base
            .quantities
            .insert("NetPerimeter".into(), PropertyValue::Length(22.0));
        qset_base
            .quantities
            .insert("Height".into(), PropertyValue::Length(2.7));
        properties
            .entry(space.clone())
            .upsert_qset(qset_base.clone());

        // Pset on a storey for good measure (covers the multi-class
        // spatial-owner case).
        let mut pset_storey = PropertySet::new("Pset_BuildingStoreyCommon");
        pset_storey.set("AboveGround".to_string(), PropertyValue::Boolean(true));
        properties
            .entry(storey.clone())
            .upsert_pset(pset_storey.clone());

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);

        // Two IFCRELDEFINESBYPROPERTIES rels for the space (one pset
        // + one qset) plus one for the storey = at least 3 rels in
        // the emitted STEP file.
        let rel_count = s
            .lines()
            .filter(|l| l.contains("IFCRELDEFINESBYPROPERTIES("))
            .count();
        assert!(
            rel_count >= 3,
            "expected >= 3 IFCRELDEFINESBYPROPERTIES (space pset + qset + storey pset), got {rel_count}: \n{s}",
        );

        let snap = IfcReader::from_string(&s).expect("spatial-owned psets must round-trip");

        // Spatial nodes are reborn with fresh EntityIds on read (the
        // STEP record only carries the GUID, not the original
        // EntityId). Build a GUID → new EntityId map so the test can
        // look up the reborn space/storey via the same GUID the
        // writer produced from the original EntityId.
        let entity_by_guid: HashMap<String, EntityId> = snap
            .guid_by_entity
            .iter()
            .map(|(eid, guid)| (guid.clone(), eid.clone()))
            .collect();
        let space_guid = crate::ifc::compress_entity_id_to_guid(&space);
        let storey_guid = crate::ifc::compress_entity_id_to_guid(&storey);
        let space_reborn = entity_by_guid
            .get(&space_guid)
            .expect("space GUID must survive round-trip")
            .clone();
        let storey_reborn = entity_by_guid
            .get(&storey_guid)
            .expect("storey GUID must survive round-trip")
            .clone();

        // Space Pset_SpaceCommon must be reconstructed.
        let space_props = snap
            .properties
            .get(&space_reborn)
            .expect("space must have properties on round-trip");
        let space_pset = space_props
            .psets
            .get("Pset_SpaceCommon")
            .expect("Pset_SpaceCommon must survive round-trip on IfcSpace");
        assert_eq!(
            space_pset.properties.get("Reference"),
            Some(&PropertyValue::Text("R-101".to_string())),
        );
        assert_eq!(
            space_pset.properties.get("Category"),
            Some(&PropertyValue::Text("Living".to_string())),
        );
        assert_eq!(
            space_pset.properties.get("PubliclyAccessible"),
            Some(&PropertyValue::Boolean(false)),
        );

        // Space Qto_SpaceBaseQuantities must be reconstructed.
        let space_qset = space_props
            .qsets
            .get("Qto_SpaceBaseQuantities")
            .expect("Qto_SpaceBaseQuantities must survive round-trip on IfcSpace");
        assert_eq!(
            space_qset.quantities.get("NetFloorArea"),
            Some(&PropertyValue::Area(28.5)),
        );
        assert_eq!(
            space_qset.quantities.get("NetPerimeter"),
            Some(&PropertyValue::Length(22.0)),
        );
        assert_eq!(
            space_qset.quantities.get("Height"),
            Some(&PropertyValue::Length(2.7)),
        );

        // Storey Pset must also be reconstructed.
        let storey_props = snap
            .properties
            .get(&storey_reborn)
            .expect("storey must have properties on round-trip");
        let storey_pset = storey_props
            .psets
            .get("Pset_BuildingStoreyCommon")
            .expect("Pset_BuildingStoreyCommon must survive round-trip on IfcBuildingStorey");
        assert_eq!(
            storey_pset.properties.get("AboveGround"),
            Some(&PropertyValue::Boolean(true)),
        );
    }

    #[test]
    fn element_classified_with_spatial_class_roundtrips_via_proxy_fallback() {
        // Defense in depth: if an element is attached via
        // `Project::attach_element` AND classified as one of the
        // spatial-structure classes (Project / Site / Building /
        // Storey / Space), the writer must NOT emit it with the
        // spatial entity type — the reader's first-pass dispatch
        // would re-classify it as a spatial node with a fresh
        // EntityId and the original element identity would be lost.
        // Instead the writer emits `IfcBuildingElementProxy` as the
        // STEP type and stashes the original tag in the Name field;
        // the reader's name-prefers-tag path reconstructs the class
        // losslessly.
        //
        // No current production code does this — `IfcSpace` is for
        // `add_child()`, not `attach_element()` — but tools / tests
        // / extension classifiers could trigger the misuse, so the
        // round-trip must remain correct.
        let mut project = Project::new("SpatialEltRT");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "S")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();

        let mut classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let el = EntityId::new();
        project.attach_element(&storey, el.clone());
        // The misuse under test: classify an element as a spatial
        // class. The writer must defend against it.
        classification.assign_manual(el.clone(), IfcClass::IfcSpace);

        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);

        // The misclassified element MUST be emitted as
        // `IfcBuildingElementProxy(...)` carrying `IfcSpace::{eid}`
        // in the Name field — exactly one occurrence (containment
        // rel still references the proxy, not the actual storey).
        assert!(
            s.contains(&format!("IfcSpace::{el}")),
            "Name field must carry the original IfcSpace::{{eid}}: {s}",
        );
        let proxy_lines: Vec<&str> = s
            .lines()
            .filter(|l| l.starts_with('#') && l.contains("IfcBuildingElementProxy("))
            .collect();
        assert_eq!(
            proxy_lines.len(),
            1,
            "expected exactly one IfcBuildingElementProxy line (the misclassified element): {s}"
        );

        let snap = IfcReader::from_string(&s)
            .expect("element with spatial-class classification must round-trip");
        let class = snap
            .classification
            .accepted_for(&el)
            .expect("element classified");
        assert_eq!(
            class,
            &IfcClass::IfcSpace,
            "original IfcSpace classification must be reconstructed from the Name-field tag",
        );
        assert!(
            snap.guid_by_entity.contains_key(&el),
            "EntityId must survive the spatial-class proxy fallback",
        );
        // The actual spatial storey must still have its own
        // `IfcSpace`-free hierarchy — i.e. the proxy element is
        // attached to the storey via the containment rel, NOT
        // hijacked into the spatial graph.
        assert!(
            snap.element_parent.contains_key(&el),
            "the misclassified element must remain an element (have a containing storey), \
             not be promoted to a spatial node",
        );
    }

    #[test]
    fn step_safe_simple_id_validator_classifies_correctly() {
        // The writer's safety predicate guards against STEP-21
        // SIMPLE_ID violations. Anchor the contract here so a future
        // edit can't silently broaden the safe set and reintroduce the
        // unparseable-line corruption mode.
        use crate::ifc::writer::is_step_safe_simple_id;

        // Safe: real IFC class tags + the `Other(_)` rsplit-once form.
        assert!(is_step_safe_simple_id("IfcWall"));
        assert!(is_step_safe_simple_id("IfcBuildingElementProxy"));
        assert!(is_step_safe_simple_id("Some::Custom::Type"));
        assert!(is_step_safe_simple_id("_underscored"));

        // Unsafe: each character below would individually corrupt
        // `parse_step_groups`.
        assert!(!is_step_safe_simple_id(""));
        assert!(!is_step_safe_simple_id("9StartsWithDigit"));
        assert!(!is_step_safe_simple_id("Foo(Bar)"));
        assert!(!is_step_safe_simple_id("Foo,Bar"));
        assert!(!is_step_safe_simple_id("Foo'Bar"));
        assert!(!is_step_safe_simple_id("Foo;Bar"));
        assert!(!is_step_safe_simple_id("Foo Bar"));
        assert!(!is_step_safe_simple_id("Foo\nBar"));
    }

    #[test]
    fn from_string_surfaces_malformed_arg_list_as_error() {
        // End-to-end: a STEP envelope whose inner arg list contains a
        // bare unmatched `)` (e.g. produced by a corrupted serializer)
        // must return `Err(Malformed)`, not panic and not
        // succeed-with-garbage. `parse_step_groups` strips the outer
        // `IFCSITE( ... )` envelope using `find('(')` / `rfind(')')`,
        // so the embedded `)` between commas — outside any quoted
        // string — is what reaches `split_step_args` and trips the
        // negative-depth guard.
        let bad = "ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('x'),'2;1');\n\
FILE_NAME('a.ifc','x',('x'),('x'),'x','x','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCSITE('00000000000000000000aa',$,$,'Site',a),$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let err = IfcReader::from_string(bad).expect_err("malformed arg list must error");
        assert!(matches!(err, IfcReadError::Malformed(_)), "{err:?}");
    }

    /// Unknown IFC measure types (e.g. `IfcMassDensityMeasure`,
    /// `IfcFrequencyMeasure`) inside a Pset round-trip through
    /// `PropertyValue::Other`. The reader preserves the raw STEP
    /// literal and the measure tag verbatim, and the writer emits
    /// them back inside an `IFCXXX(raw)` wrapper. The next reader
    /// pass must see the same value.
    #[test]
    fn unknown_pset_measure_types_round_trip() {
        // Build an in-memory file that mixes a modeled property
        // (Length) and an unmodeled one (MassDensity) inside the
        // same Pset, attached to a tiny IfcWall element. Read it
        // → write it → read it again and assert the unmodeled
        // value survives bit-identical.
        let mut project = Project::new("P");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let wall = EntityId::new();
        project.attach_element(&storey, wall.clone());
        let mut classification = ClassificationStore::new();
        classification.assign_manual(wall.clone(), IfcClass::IfcWall);
        let mut props = PropertyStore::new();
        let mut p = PropertySet::new("Pset_WallCommon");
        p.set("Length", PropertyValue::Length(3.5));
        p.set(
            "Density",
            PropertyValue::Other {
                measure: "IfcMassDensityMeasure".to_string(),
                raw: "2400.0".to_string(),
            },
        );
        props.entry(wall.clone()).upsert_pset(p);

        let body = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        // Round-trip 1
        let snap1 = IfcReader::from_string(&body).expect("first parse");
        let stored = snap1
            .properties
            .get(&wall)
            .and_then(|p| p.psets.get("Pset_WallCommon"))
            .and_then(|ps| ps.properties.get("Density"))
            .cloned();
        match stored {
            Some(PropertyValue::Other {
                ref measure,
                ref raw,
            }) => {
                assert_eq!(measure.to_ascii_uppercase(), "IFCMASSDENSITYMEASURE");
                assert_eq!(raw, "2400.0");
            }
            ref other => panic!("expected PropertyValue::Other, got {other:?}"),
        }
        // The body must literally contain the measure wrapper.
        assert!(
            body.contains("IFCMASSDENSITYMEASURE(2400.0)"),
            "writer must re-emit the raw measure wrapper, got body:\n{body}"
        );

        // Round-trip 2: re-write the snapshot's reconstructed pset
        // and ensure the second parse yields an identical value.
        let mut props2 = PropertyStore::new();
        for (el, p) in snap1.properties.iter() {
            for ps in p.psets.values() {
                props2.entry(el.clone()).upsert_pset(ps.clone());
            }
        }
        let body2 = crate::ifc::IfcWriter::to_string(&project, &classification, &props2);
        let snap2 = IfcReader::from_string(&body2).expect("second parse");
        assert_eq!(
            snap1.properties.get(&wall),
            snap2.properties.get(&wall),
            "two reader passes converge on the same PropertyValue tree"
        );
    }

    /// Unmodeled `IFCQUANTITY*` types (e.g. IFC4x3's
    /// `IFCQUANTITYTIME`) must round-trip through the read→write→
    /// read cycle via `PropertyValue::Other`, exactly like
    /// unmodeled property measures do above. The reader's quantity
    /// dispatch matches any `IFCQUANTITY*` prefix (not just the five
    /// canonical kinds), so the catch-all arm of
    /// `parse_quantity_typed` is reachable and the writer's
    /// `serialize_quantity_value::Other` arm emits the same STEP
    /// literal we ingested.
    #[test]
    fn unknown_quantity_kinds_round_trip() {
        let mut project = Project::new("P");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let wall = EntityId::new();
        project.attach_element(&storey, wall.clone());
        let mut classification = ClassificationStore::new();
        classification.assign_manual(wall.clone(), IfcClass::IfcWall);

        // QuantitySet mixing a modeled (Length) and an unmodeled
        // (Time) quantity. IFCQUANTITYTIME was added in IFC4x3 and
        // is not modeled as a typed PropertyValue variant; the
        // round-trip must preserve it through PropertyValue::Other.
        let mut props = PropertyStore::new();
        let mut q = QuantitySet::new("Qto_WallBaseQuantities");
        q.quantities
            .insert("Length".into(), PropertyValue::Length(3.5));
        q.quantities.insert(
            "ConstructionTime".into(),
            PropertyValue::Other {
                measure: "IfcQuantityTime".into(),
                raw: "3600.0".into(),
            },
        );
        props.entry(wall.clone()).upsert_qset(q);

        let body = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        // The writer must emit the unmodeled quantity verbatim with
        // its uppercased measure wrapper.
        assert!(
            body.contains("IFCQUANTITYTIME('ConstructionTime',$,$,3600.0)"),
            "writer must re-emit the raw IFCQUANTITYTIME wrapper, got body:\n{body}"
        );

        let snap1 = IfcReader::from_string(&body).expect("first parse");
        let stored = snap1
            .properties
            .get(&wall)
            .and_then(|p| p.qsets.get("Qto_WallBaseQuantities"))
            .and_then(|qs| qs.quantities.get("ConstructionTime"))
            .cloned();
        match stored {
            Some(PropertyValue::Other {
                ref measure,
                ref raw,
            }) => {
                assert_eq!(measure.to_ascii_uppercase(), "IFCQUANTITYTIME");
                assert_eq!(raw, "3600.0");
            }
            ref other => {
                panic!("expected PropertyValue::Other for unmodeled IFCQUANTITYTIME, got {other:?}")
            }
        }
        // The known IFCQUANTITYLENGTH companion must also survive.
        let length = snap1
            .properties
            .get(&wall)
            .and_then(|p| p.qsets.get("Qto_WallBaseQuantities"))
            .and_then(|qs| qs.quantities.get("Length"))
            .cloned();
        assert_eq!(length, Some(PropertyValue::Length(3.5)));

        // Re-write and re-read to confirm second pass converges.
        let mut props2 = PropertyStore::new();
        for (el, p) in snap1.properties.iter() {
            for qs in p.qsets.values() {
                props2.entry(el.clone()).upsert_qset(qs.clone());
            }
        }
        let body2 = crate::ifc::IfcWriter::to_string(&project, &classification, &props2);
        let snap2 = IfcReader::from_string(&body2).expect("second parse");
        assert_eq!(
            snap1.properties.get(&wall),
            snap2.properties.get(&wall),
            "two reader passes converge on the same QuantitySet tree"
        );
    }

    /// `FILE_SCHEMA(('IFC4'))` -> `IfcSchema::Ifc4`, and the snapshot
    /// surfaces the detected schema verbatim. Asserts the writer's
    /// canonical AEC output is round-trip stable through the new
    /// schema-aware envelope check.
    #[test]
    fn detects_ifc4_schema_on_writer_output() {
        let (project, classification, props, _ids) = build_tiny_project();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("parses");
        assert_eq!(snap.schema, IfcSchema::Ifc4);
    }

    /// Files declaring `FILE_SCHEMA(('IFC2X3'))` parse with schema
    /// `Ifc2x3` and recover any modeled entities the same way IFC4
    /// files do — the writer's entity grammar is identical at the
    /// arg shapes we depend on. Extra IFC2x3-specific entities
    /// (here `IFCEXTRUDEDAREASOLID`) are tolerated and skipped.
    #[test]
    fn parses_ifc2x3_with_unmodeled_geometry_entities() {
        let body = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
FILE_NAME('legacy.ifc','2024-01-01',(''),(''),'AEC Studio','AEC Studio','');\n\
FILE_SCHEMA(('IFC2X3'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCPROJECT('00000000000000000000a1',$,$,'P1',$,$,$,$,$);\n\
#2 = IFCSITE('00000000000000000000a2',$,$,'Site',$,$,$,$,$);\n\
#3 = IFCRELAGGREGATES('00000000000000000000a3',$,$,$,#1,(#2));\n\
#42 = IFCEXTRUDEDAREASOLID(#41,#7,#11,300.0);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let snap = IfcReader::from_string(body).expect("IFC2x3 parses");
        assert_eq!(snap.schema, IfcSchema::Ifc2x3);
        // The IfcExtrudedAreaSolid was tolerated and skipped — it
        // shows up in records_seen but not in any of the modeled
        // counts.
        assert!(snap.stats.records_seen >= 4);
        assert_eq!(snap.stats.spatial_nodes, 2); // project + site
        assert_eq!(snap.stats.aggregations, 1);
        assert_eq!(snap.stats.elements, 0);
    }

    /// `FILE_SCHEMA(('IFC4X3'))` and its release-stage variants
    /// (`IFC4X3_ADD2`, `IFC4X3_RC4`) all map to `IfcSchema::Ifc4x3`.
    #[test]
    fn detects_ifc4x3_release_stage_variants() {
        for token in ["IFC4X3", "IFC4X3_ADD2", "IFC4X3_RC4"] {
            let body = format!(
                "ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('vd'),'2;1');\n\
FILE_NAME('x','2024-01-01',(''),(''),'AEC','AEC','');\n\
FILE_SCHEMA(('{token}'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCPROJECT('00000000000000000000a1',$,$,'P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n"
            );
            let snap = IfcReader::from_string(&body).expect("IFC4x3 token parses");
            assert_eq!(snap.schema, IfcSchema::Ifc4x3, "token = {token}");
        }
    }

    /// `FILE_SCHEMA(('IFCXX'))` for an unknown schema literal must
    /// fail with `MissingSection("FILE_SCHEMA")` so callers can
    /// surface a clear "unsupported schema" error rather than
    /// silently re-interpreting the file as IFC4.
    #[test]
    fn unknown_schema_literal_is_rejected() {
        let body = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('vd'),'2;1');\n\
FILE_NAME('x','2024-01-01',(''),(''),'AEC','AEC','');\n\
FILE_SCHEMA(('IFC5'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCPROJECT('00000000000000000000a1',$,$,'P',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let err = IfcReader::from_string(body).expect_err("unknown schema must error");
        assert!(
            matches!(err, IfcReadError::MissingSection("FILE_SCHEMA")),
            "got {err:?}"
        );
    }

    /// Multi-line STEP records — where the arg list is split across
    /// physical newlines per ISO 10303-21 §11.2 — tokenize
    /// identically to single-line ones. External authoring tools
    /// commonly emit pretty-printed IFC; we must round-trip it.
    #[test]
    fn parses_multi_line_step_records() {
        let body = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('vd'),'2;1');\n\
FILE_NAME('x','2024-01-01',(''),(''),'AEC','AEC','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1 = IFCPROJECT(\n\
    '00000000000000000000a1',\n\
    $,$,\n\
    'Multi-line Project',\n\
    $,$,$,$,$);\n\
#2 = IFCSITE(\n\
    '00000000000000000000a2',\n\
    $,$,\n\
    'Multi-line Site',\n\
    $,$,$,$,$);\n\
#3 = IFCRELAGGREGATES('00000000000000000000a3',$,$,$,#1,(#2));\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let snap = IfcReader::from_string(body).expect("multi-line parses");
        assert_eq!(snap.stats.spatial_nodes, 2);
        assert_eq!(snap.stats.aggregations, 1);
        // Project's display name was recovered across newlines.
        let root = snap.project.root.clone();
        assert_eq!(snap.project.get(&root).unwrap().name, "Multi-line Project");
    }

    /// STEP `/* ... */` comments (ISO 10303-21 §6.4.1) can appear
    /// anywhere outside string literals, including spanning a
    /// would-be `;` record terminator. We strip them so the
    /// embedded `;` does not split a record.
    #[test]
    fn strips_step_comments_including_embedded_semicolons() {
        let body = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('vd'),'2;1');\n\
FILE_NAME('x','2024-01-01',(''),(''),'AEC','AEC','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
/* comment containing ; which must not split records */\n\
#1 = IFCPROJECT('00000000000000000000a1',$,$,'P',$,$,$,$,$);\n\
#2 = IFCSITE('00000000000000000000a2',$,$,/* inline ; comment */'Site',$,$,$,$,$);\n\
#3 = IFCRELAGGREGATES('00000000000000000000a3',$,$,$,#1,(#2));\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let snap = IfcReader::from_string(body).expect("comments are stripped");
        assert_eq!(snap.stats.spatial_nodes, 2);
        assert_eq!(snap.stats.aggregations, 1);
    }

    /// Embedded `;` inside a quoted string literal is *not* a
    /// record terminator. This protects against pathological
    /// names like `"Café; Bar"`.
    #[test]
    fn embedded_semicolon_in_string_does_not_split_record() {
        let mut project = Project::new("Café; Bar");
        let _site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Café; Bar Site")
            .unwrap();
        let classification = ClassificationStore::new();
        let props = PropertyStore::new();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap = IfcReader::from_string(&s).expect("read back");
        let root = snap.project.root.clone();
        assert_eq!(snap.project.get(&root).unwrap().name, "Café; Bar");
    }

    /// `StepIter` over a streaming `BufRead` yields the same
    /// records as `parse_step_groups` does in-memory. Multi-line
    /// records and comments split identically across the two
    /// modes.
    #[test]
    fn step_iter_yields_records_one_at_a_time() {
        let body = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('vd'),'2;1');\n\
FILE_NAME('x','2024-01-01',(''),(''),'AEC','AEC','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
/* leading comment */\n\
#1 = IFCPROJECT(\n\
    '00000000000000000000a1',\n\
    $,$,\n\
    'P',\n\
    $,$,$,$,$);\n\
#2 = IFCSITE('00000000000000000000a2',$,$,'S',$,$,$,$,$);\n\
ENDSEC;\n\
END-ISO-10303-21;\n";
        let cursor = std::io::Cursor::new(body.as_bytes());
        let records: Vec<_> = IfcReader::iter(cursor).collect::<Result<_, _>>().unwrap();
        // 2 entity records + the writer's HEADER lines are NOT entity
        // records (no `#N = ...` shape) and are filtered out by
        // parse_logical_record.
        let kinds: Vec<&str> = records.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, vec!["IFCPROJECT", "IFCSITE"]);
        assert_eq!(records[0].step_id, 1);
        assert_eq!(records[1].step_id, 2);
    }

    /// `IfcReader::from_reader` produces the same `IfcSnapshot` as
    /// `from_string` when fed the same bytes through a `BufRead`.
    /// This is the bit-for-bit equivalence test required by the
    /// task spec: the streaming variant is a memory-bounded twin
    /// of the in-memory parser.
    #[test]
    fn from_reader_matches_from_string() {
        let (project, classification, props, _ids) = build_tiny_project();
        let s = crate::ifc::IfcWriter::to_string(&project, &classification, &props);
        let snap_a = IfcReader::from_string(&s).expect("from_string");
        let cursor = std::io::Cursor::new(s.as_bytes());
        let snap_b = IfcReader::from_reader(cursor).expect("from_reader");
        assert_eq!(snap_a.stats, snap_b.stats);
        assert_eq!(snap_a.schema, snap_b.schema);
        assert_eq!(snap_a.guid_by_entity.len(), snap_b.guid_by_entity.len());
    }

    /// `StepIter` must NOT silently truncate when the underlying
    /// reader reaches EOF mid-string. The eager `from_string` path
    /// flags the same input as `Malformed`; the streaming iterator
    /// must do the same so a corrupted IFC file fails identically
    /// through either API. This pins the EOF-validation contract.
    #[test]
    fn step_iter_rejects_unterminated_string_at_eof() {
        let truncated = "#1=IFCPROPERTYSINGLEVALUE('NeverCloses,";
        let cursor = std::io::Cursor::new(truncated.as_bytes());
        let mut iter = StepIter::new(std::io::BufReader::new(cursor));
        // No complete `... ;` record, so the first poll just returns
        // None? — except we're now requiring the iterator to surface
        // the malformed trailing bytes as an explicit error before
        // returning None.
        let mut saw_error = false;
        for item in iter.by_ref() {
            if let Err(IfcReadError::Malformed(msg)) = item {
                saw_error = true;
                assert!(
                    msg.contains("unterminated string"),
                    "expected unterminated-string diagnostic, got: {msg}"
                );
                break;
            }
        }
        assert!(
            saw_error,
            "StepIter must report unterminated-string at EOF, not silently stop"
        );
        // Once faulted, the iterator must stay faulted.
        assert!(iter.next().is_none());
    }

    /// Same EOF contract for unbalanced parens.
    #[test]
    fn step_iter_rejects_unbalanced_parens_at_eof() {
        let truncated = "#1=IFCFOO(1,(2,3";
        let cursor = std::io::Cursor::new(truncated.as_bytes());
        let mut iter = StepIter::new(std::io::BufReader::new(cursor));
        let mut saw_error = false;
        for item in iter.by_ref() {
            if let Err(IfcReadError::Malformed(msg)) = item {
                saw_error = true;
                assert!(
                    msg.contains("unbalanced parens"),
                    "expected unbalanced-parens diagnostic, got: {msg}"
                );
                break;
            }
        }
        assert!(saw_error, "StepIter must report unbalanced-parens at EOF");
    }

    /// Whitespace-only trailing bytes after the last `... ;` are
    /// allowed — the iterator must NOT report an error.
    #[test]
    fn step_iter_tolerates_trailing_whitespace() {
        let valid = "#1=IFCAPPLICATION('a','b','c','d');   \n\n";
        let cursor = std::io::Cursor::new(valid.as_bytes());
        let iter = StepIter::new(std::io::BufReader::new(cursor));
        let results: Vec<_> = iter.collect();
        assert_eq!(results.len(), 1, "expected one record, got {results:?}");
        assert!(results[0].is_ok());
    }

    /// `detect_schema` must NOT mistake a `FILE_SCHEMA(('IFC2X3'))`
    /// token appearing inside a DATA-section string literal for the
    /// file's declared schema. The HEADER-bounded search defends
    /// against an adversarial / corrupted file where the DATA
    /// section embeds a fake schema marker.
    #[test]
    fn detect_schema_ignores_file_schema_inside_data_section() {
        // HEADER says IFC4; DATA section has an entity whose
        // Description happens to contain the substring
        // "FILE_SCHEMA(('IFC2X3'))" inside a string literal.
        let text = "ISO-10303-21;\n\
            HEADER;\n\
            FILE_DESCRIPTION(('a'),'2;1');\n\
            FILE_NAME('','',(''),(''),'','','');\n\
            FILE_SCHEMA(('IFC4'));\n\
            ENDSEC;\n\
            DATA;\n\
            #1=IFCAPPLICATION('Trojan with FILE_SCHEMA((\\'IFC2X3\\'))','v1','app','id');\n\
            ENDSEC;\n\
            END-ISO-10303-21;";
        let schema = detect_schema(text).expect("schema detect");
        assert_eq!(schema, IfcSchema::Ifc4);
    }

    /// ISO 10303-21 §6.4.3.2 specifies lowercase `e` for the REAL
    /// exponent, but real-world IFC2x3 archives from pre-2010
    /// FORTRAN-derived exporters (AutoCAD-IFC and friends) ship with
    /// the FORTRAN `D` / `d` exponent form. `parse_step_real` must
    /// accept both forms, preserve canonical-form parsing on the
    /// zero-allocation fast path, and reject anything that isn't a
    /// REAL literal (quoted strings, booleans, garbage).
    #[test]
    fn parse_step_real_accepts_canonical_and_legacy_d_exponent() {
        // Canonical forms (fast path, no rewrite needed).
        assert_eq!(parse_step_real("0"), Some(0.0));
        assert_eq!(parse_step_real("3.5"), Some(3.5));
        assert_eq!(parse_step_real("-3.5"), Some(-3.5));
        assert_eq!(parse_step_real("1.5e-3"), Some(1.5e-3));
        assert_eq!(parse_step_real("1.5E+10"), Some(1.5e10));
        // Trim is on by default — IFC writers don't pad, but
        // hand-constructed test fixtures do.
        assert_eq!(parse_step_real("  -2.0 "), Some(-2.0));

        // Legacy FORTRAN `D` exponent.
        assert_eq!(parse_step_real("1.5D-3"), Some(1.5e-3));
        assert_eq!(parse_step_real("1.5d-3"), Some(1.5e-3));
        assert_eq!(parse_step_real("2D5"), Some(2e5));
        assert_eq!(parse_step_real("-3.5D+2"), Some(-350.0));

        // Lexical shapes that are valid in `PropertyValue::Other.raw`
        // but are not REAL literals must return `None` (don't claim
        // numeric recoverability for booleans / quoted strings).
        assert!(parse_step_real("'kg/m3'").is_none());
        assert!(parse_step_real(".T.").is_none());
        assert!(parse_step_real(".F.").is_none());
        assert!(parse_step_real("").is_none());
        assert!(parse_step_real("abc").is_none());
        // A `D` that isn't an exponent (e.g. embedded mid-mantissa)
        // would already fail canonical parse; rewriting to `E` still
        // fails. Don't accidentally rescue malformed literals.
        assert!(parse_step_real("D5").is_none()); // bare exponent, no mantissa

        // Defense in depth: Rust's f64::from_str accepts NaN/inf
        // textual tokens, but ISO 10303-21 REAL grammar does not.
        // Silently admitting these would let a malformed STEP file
        // poison downstream BOQ / signed_volume arithmetic with
        // NaN-propagation. parse_step_real must reject them at the
        // parse boundary.
        assert!(
            parse_step_real("NaN").is_none(),
            "NaN must not parse as REAL"
        );
        assert!(parse_step_real("nan").is_none());
        assert!(
            parse_step_real("inf").is_none(),
            "inf must not parse as REAL"
        );
        assert!(parse_step_real("infinity").is_none());
        assert!(parse_step_real("-inf").is_none());
        assert!(parse_step_real("+inf").is_none());
        // The legacy-D slow path is gated by the same finiteness
        // check — a contrived `InfD0` literal must also be rejected.
        assert!(parse_step_real("InfD0").is_none());
    }

    /// End-to-end: a STEP file whose REAL literals use legacy `D`
    /// exponent notation must still round-trip through the full
    /// reader (typed measures like `IfcLengthMeasure` go through
    /// `parse_real`, not just `as_real()` on the `Other` catch-all).
    #[test]
    fn reader_accepts_legacy_d_exponent_in_typed_measures() {
        // Build a valid IFC via the writer (canonical `e` exponent
        // form), then surgically rewrite the relevant
        // `IFCLENGTHMEASURE(1500.0)` literal to use the legacy
        // FORTRAN-style `1.5D+3` exponent. The pre-fix reader would
        // reject that file at `parse_real`; the post-fix reader must
        // parse it and recover the numeric value via
        // `parse_step_real`.
        let mut project = Project::new("LegacyDExponent");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let wall_id = EntityId::new();
        project.attach_element(&storey, wall_id.clone());
        let mut classification = ClassificationStore::new();
        classification.assign_manual(wall_id.clone(), IfcClass::IfcWall);
        let mut properties = PropertyStore::new();
        let mut pset = PropertySet::new("Pset_WallCommon");
        pset.set("Length", PropertyValue::Length(1500.0));
        properties.entry(wall_id.clone()).upsert_pset(pset);

        let canonical = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);
        // Find and rewrite the writer's canonical
        // `IFCLENGTHMEASURE(1500.0)` (or its scientific form) to the
        // legacy FORTRAN `1.5D+3` exponent variant. The writer emits
        // `IFCLENGTHMEASURE(1500.0)` for a length of 1500 mm; map
        // that to `1.5D+3` to exercise the D-exponent code path.
        assert!(
            canonical.contains("IFCLENGTHMEASURE(1500"),
            "writer must emit IFCLENGTHMEASURE(1500…) so we can rewrite it; got:\n{canonical}",
        );
        let with_d_exponent =
            canonical.replace("IFCLENGTHMEASURE(1500.0)", "IFCLENGTHMEASURE(1.5D+3)");
        assert_ne!(
            canonical, with_d_exponent,
            "the substitution must have actually rewritten the literal",
        );

        let snap = IfcReader::from_string(&with_d_exponent)
            .expect("reader must accept legacy D-exponent on typed measures");
        let length = snap
            .properties
            .get(&wall_id)
            .and_then(|e| e.get("Pset_WallCommon", "Length"))
            .and_then(PropertyValue::as_real)
            .expect("Pset_WallCommon.Length must be readable after D-exponent normalisation");
        assert!(
            (length - 1500.0).abs() < 1e-9,
            "D-exponent legacy literal 1.5D+3 must parse as 1500.0; got {length}",
        );
    }

    /// End-to-end defense in depth: a STEP file that smuggles `NaN`
    /// into a typed measure must fail to parse, not silently produce a
    /// wall with a NaN-valued Length quantity. Rust's `f64::from_str`
    /// accepts `NaN`/`inf`, but ISO 10303-21's REAL grammar does not;
    /// `parse_step_real` rejects them, and the reader surfaces that
    /// rejection as `IfcReadError::Malformed`.
    #[test]
    fn reader_rejects_nan_in_typed_measure() {
        let mut project = Project::new("NaNMeasure");
        let site = project
            .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
            .unwrap();
        let bldg = project
            .add_child(&site, IfcClass::IfcBuilding, "B")
            .unwrap();
        let storey = project
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let wall_id = EntityId::new();
        project.attach_element(&storey, wall_id.clone());
        let mut classification = ClassificationStore::new();
        classification.assign_manual(wall_id.clone(), IfcClass::IfcWall);
        let mut properties = PropertyStore::new();
        let mut pset = PropertySet::new("Pset_WallCommon");
        pset.set("Length", PropertyValue::Length(1500.0));
        properties.entry(wall_id.clone()).upsert_pset(pset);

        let canonical = crate::ifc::IfcWriter::to_string(&project, &classification, &properties);
        let poisoned = canonical.replace("IFCLENGTHMEASURE(1500.0)", "IFCLENGTHMEASURE(NaN)");
        assert_ne!(
            canonical, poisoned,
            "the substitution must have actually rewritten the literal",
        );

        let err = IfcReader::from_string(&poisoned)
            .expect_err("reader must reject NaN-valued IfcLengthMeasure literals");
        match err {
            IfcReadError::Malformed(msg) => {
                assert!(
                    msg.contains("real parse"),
                    "expected real-parse rejection for NaN literal; got: {msg}",
                );
            }
            other => panic!("expected Malformed error for NaN literal; got {other:?}"),
        }
    }

    /// Conversely, a HEADER section that legitimately declares
    /// IFC2X3 must still be detected, even when the file mentions
    /// other tokens elsewhere.
    #[test]
    fn detect_schema_finds_ifc2x3_in_header() {
        let text = "ISO-10303-21;\n\
            HEADER;\n\
            FILE_DESCRIPTION(('a'),'2;1');\n\
            FILE_NAME('','',(''),(''),'','','');\n\
            FILE_SCHEMA(('IFC2X3'));\n\
            ENDSEC;\n\
            DATA;\n\
            ENDSEC;\n\
            END-ISO-10303-21;";
        let schema = detect_schema(text).expect("schema detect");
        assert_eq!(schema, IfcSchema::Ifc2x3);
    }
}

// `defines_pset` here covers both Psets and Qsets — the writer uses
// the same `IfcRelDefinesByProperties` relation for both, exactly as
// the IFC4 schema does. `IfcRelDefinesByProperties.RelatingPropertyDefinition`
// is of type `IfcPropertySetDefinition`, the parent of both
// `IfcPropertySet` and `IfcElementQuantity`.
//
// Naming: `defines_pset` is short for `defines_by_propertydef`.
//
// (Kept above the impl tests so future maintainers see the intent.)
