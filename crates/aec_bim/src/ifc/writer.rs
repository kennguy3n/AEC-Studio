//! IFC4 STEP writer for the AEC Studio in-process IFC pipeline.
//!
//! Serialises a [`Project`] plus its [`ClassificationStore`] and
//! [`PropertyStore`] into a minimal but real IFC4 STEP byte stream:
//!
//!   * ISO-10303-21 header with `FILE_SCHEMA(('IFC4'))`.
//!   * Spatial structure (Project → Site → Building → Storey → Space)
//!     emitted as `IfcProject` / `IfcSite` / `IfcBuilding` /
//!     `IfcBuildingStorey` / `IfcSpace` instances.
//!   * Spatial-aggregation relations as `IfcRelAggregates`.
//!   * Elements attached to a storey as `IfcWall`, `IfcSlab`, etc.
//!     wired into the storey via `IfcRelContainedInSpatialStructure`.
//!   * Each element's Psets serialised as `IfcPropertySingleValue`
//!     entries on an `IfcPropertySet`, wired in via
//!     `IfcRelDefinesByProperties`.
//!   * Each element's Quantity Sets (Qtos) serialised as
//!     `IfcQuantityLength` / `IfcQuantityArea` / `IfcQuantityVolume`
//!     / `IfcQuantityCount` entries on an `IfcElementQuantity`, wired
//!     in via `IfcRelDefinesByProperties`.
//!   * Every IFC entity carries a stable 22-char `IfcGloballyUniqueId`
//!     (either the node's pre-assigned `ifc_guid` or one
//!     deterministically derived from the `EntityId` via
//!     [`super::compress_entity_id_to_guid`]).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use thiserror::Error;

use aec_core::types::EntityId;

use crate::classification::{ClassificationStore, IfcClass};
use crate::materials::{MaterialAssignment, MaterialStore};
use crate::properties::{PropertyStore, PropertyValue};
use crate::spatial::Project;

use super::{compress_entity_id_to_guid, derive_guid_from_str, sanitize_ifc_guid};

#[derive(Debug, Error)]
pub enum IfcWriteError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct IfcWriter;

struct StepBuf {
    inner: Vec<u8>,
    next_id: u32,
}

impl StepBuf {
    fn new() -> Self {
        Self {
            inner: Vec::new(),
            next_id: 1,
        }
    }

    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write_line(&mut self, id: u32, body: impl AsRef<str>) {
        let body = body.as_ref();
        self.inner
            .extend_from_slice(format!("#{id} = {body};\n").as_bytes());
    }
}

impl IfcWriter {
    /// Serialise the project + classification + property graph to an
    /// IFC4 STEP byte stream. The material library is omitted — use
    /// [`IfcWriter::to_string_with_materials`] when the caller wants
    /// `IfcMaterial` / `IfcMaterialLayerSet` / `IfcRelAssociatesMaterial`
    /// records in the output. This 3-arg entry stays exactly as-is
    /// for the test suite and existing call-sites that don't care
    /// about materials.
    pub fn to_string(
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
    ) -> String {
        Self::to_string_with_materials(
            project,
            classification,
            properties,
            &MaterialStore::default(),
        )
    }

    /// Serialise project + classification + properties + materials
    /// to an IFC4 STEP byte stream.
    ///
    /// Materials are emitted in dependency order so the STEP cross-
    /// reference graph is satisfiable on a single forward pass:
    /// `IfcMaterial` → `IfcMaterialLayer` → `IfcMaterialLayerSet`
    /// → `IfcRelAssociatesMaterial`. Element step ids are pulled
    /// from the same `element_step` map that already drives Pset
    /// rels, so the rel target list always references previously-
    /// emitted entities.
    pub fn to_string_with_materials(
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
        materials: &MaterialStore,
    ) -> String {
        let mut buf = StepBuf::new();

        // ---- Header ----
        let header = "\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');\n\
FILE_NAME('aec_studio.ifc','2026-05-20T00:00:00',('AEC Studio'),('Studio'),'AEC Studio','AEC Studio','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n";
        buf.inner.extend_from_slice(header.as_bytes());

        // Reserve a global owner-history entry once and reuse it.
        let owner_history = buf.alloc();
        buf.write_line(
            owner_history,
            "IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200)",
        );

        // ---- Spatial graph (DFS from root) ----
        //
        // We collect the visited order into `spatial_order` so the
        // subsequent aggregation- and containment-relation loops can
        // iterate the spatial tree in deterministic DFS order rather
        // than HashMap iteration order. Correctness doesn't depend on
        // it (the reader cross-references by `#N`), but byte-identical
        // output matters for fingerprinting, golden tests, and
        // content-addressed export caches.
        let mut spatial_step: HashMap<EntityId, u32> = HashMap::new();
        let mut spatial_order: Vec<EntityId> = Vec::new();
        let mut stack = vec![project.root.clone()];
        while let Some(id) = stack.pop() {
            let Some(node) = project.nodes.get(&id) else {
                continue;
            };
            // Sanitize any user-supplied GUID so we never interpolate
            // an embedded `'`, `\`, control char, or wrong-length value
            // into the STEP single-quoted string (which would corrupt
            // the file and break the reader's tokenizer).
            let guid = match node.ifc_guid.as_deref() {
                Some(g) => sanitize_ifc_guid(g, &id.to_string()),
                None => compress_entity_id_to_guid(&id),
            };
            let step_id = buf.alloc();
            spatial_step.insert(id.clone(), step_id);
            spatial_order.push(id.clone());
            // Spatial-structure classes (IfcProject/Site/Building/Storey/
            // Space) are baked into the IfcClass enum and never originate
            // from user-supplied strings, so their `ifc_tag()` is always a
            // valid STEP SIMPLE_ID. We still route the *Name* field
            // through `escape_step_string` for defense in depth (e.g. a
            // user-supplied space label containing `'` or `\n`), but no
            // STEP-type sanitisation is required here.
            let tag = node.class.ifc_tag();
            // IfcProject has an extra (RepContext, UnitAssign) slot; we
            // pass placeholders ($) so the round-trip stays
            // schema-shaped without geometry context.
            match node.class {
                IfcClass::IfcProject => {
                    buf.write_line(
                        step_id,
                        format!(
                            "{tag}('{guid}',#{owner},$,'{name}',$,$,$,$,$)",
                            owner = owner_history,
                            name = escape_step_string(&node.name),
                        ),
                    );
                }
                _ => {
                    buf.write_line(
                        step_id,
                        format!(
                            "{tag}('{guid}',#{owner},$,'{name}',$,$,$,$)",
                            owner = owner_history,
                            name = escape_step_string(&node.name),
                        ),
                    );
                }
            }
            for child in node.children.iter().rev() {
                stack.push(child.clone());
            }
        }

        // ---- Spatial aggregation: parent IFCRELAGGREGATES list of children ----
        //
        // Walk `spatial_order` (deterministic DFS) rather than the
        // backing HashMap so the emitted aggregation rels appear in a
        // reproducible order across runs.
        for id in &spatial_order {
            let Some(node) = project.nodes.get(id) else {
                continue;
            };
            if node.children.is_empty() {
                continue;
            }
            let Some(parent_step) = spatial_step.get(id) else {
                continue;
            };
            let child_refs: Vec<String> = node
                .children
                .iter()
                .filter_map(|c| spatial_step.get(c).map(|sid| format!("#{sid}")))
                .collect();
            if child_refs.is_empty() {
                continue;
            }
            let rel = buf.alloc();
            let rel_guid = derive_guid_from_str(&format!("rel-agg::{}", id));
            buf.write_line(
                rel,
                format!(
                    "IFCRELAGGREGATES('{rel_guid}',#{owner},$,$,#{parent},({children}))",
                    owner = owner_history,
                    parent = parent_step,
                    children = child_refs.join(",")
                ),
            );
        }

        // ---- Elements + IFCRELCONTAINEDINSPATIALSTRUCTURE per storey ----
        //
        // Same deterministic-DFS iteration as the aggregation pass —
        // ensures contained-element rels (and the element STEP records
        // they reference) emit in stable storey order.
        let mut element_step: HashMap<EntityId, u32> = HashMap::new();
        for storey_id in &spatial_order {
            let Some(node) = project.nodes.get(storey_id) else {
                continue;
            };
            if node.elements.is_empty() {
                continue;
            }
            let Some(storey_step) = spatial_step.get(storey_id) else {
                continue;
            };
            let mut contained_refs: Vec<String> = Vec::with_capacity(node.elements.len());
            for el in &node.elements {
                let class = classification
                    .accepted_for(el)
                    .cloned()
                    .unwrap_or(IfcClass::IfcFurnishingElement);
                let guid = compress_entity_id_to_guid(el);
                let step_id = buf.alloc();
                element_step.insert(el.clone(), step_id);
                // The original tag preserves the case for `Other(s)` so
                // the reader can reconstruct it verbatim; the *emitted*
                // STEP type must additionally be a valid STEP SIMPLE_ID
                // (no `(`, `)`, `'`, `,`, `;`, whitespace, or control
                // chars), otherwise the reader's `parse_step_groups`
                // would split the line at the first stray `(` and treat
                // the rest as garbage. For unsafe `Other(...)` strings we
                // fall back to the canonical IFC catch-all type
                // `IfcBuildingElementProxy`; the original tag is still
                // carried in the Name field as `{original_tag}::{eid}`
                // so the reader's name-prefers-tag path reconstructs
                // `Other(original_tag)` losslessly.
                let original_tag = class.ifc_tag();
                // Two reasons to fall back to `IfcBuildingElementProxy`
                // as the STEP entity type:
                //   1. The tag isn't a valid STEP SIMPLE_ID — e.g. an
                //      extension classifier produced
                //      `Other("Foo(Bar)")` and emitting it verbatim
                //      would corrupt `parse_step_groups`.
                //   2. The tag IS a STEP-safe identifier but it names
                //      one of the IFC4 spatial-structure classes
                //      (Project / Site / Building / Storey / Space).
                //      The reader dispatches those into the spatial
                //      branch on first pass and never tries to decode
                //      the embedded `{tag}::{eid}`, so the original
                //      EntityId would be lost on round-trip.
                // In both cases the Name field still carries
                // `{original_tag}::{eid}` and the reader's
                // name-prefers-tag path reconstructs the class
                // losslessly.
                let step_type_tag =
                    if is_step_safe_simple_id(original_tag) && !is_spatial_ifc_class(&class) {
                        original_tag
                    } else {
                        "IfcBuildingElementProxy"
                    };
                buf.write_line(
                    step_id,
                    format!(
                        "{step_type_tag}('{guid}',#{owner},$,'{name}',$,$,$,$,$)",
                        owner = owner_history,
                        // Always route the composed name through
                        // `escape_step_string` — for the unsafe-tag
                        // fallback this is the *only* place the
                        // original tag survives, so corruption here
                        // would lose the `Other(...)` payload on
                        // re-import.
                        name = escape_step_string(&format!("{original_tag}::{el}")),
                    ),
                );
                contained_refs.push(format!("#{step_id}"));
            }
            if !contained_refs.is_empty() {
                let rel = buf.alloc();
                let rel_guid = derive_guid_from_str(&format!("rel-contained::{storey_id}"));
                buf.write_line(
                    rel,
                    format!(
                        "IFCRELCONTAINEDINSPATIALSTRUCTURE('{rel_guid}',#{owner},$,$,({children}),#{storey})",
                        owner = owner_history,
                        children = contained_refs.join(","),
                        storey = storey_step,
                    ),
                );
            }
        }

        // ---- Psets + Qtos ----
        //
        // Per IFC4 schema, `IfcRelDefinesByProperties.RelatedObjects`
        // is `SET[1:?] OF IfcObjectDefinition` — i.e. both building
        // elements (`IfcElement` subtypes) AND spatial structure
        // elements (`IfcSpatialStructureElement` subtypes:
        // `IfcSpace` / `IfcBuildingStorey` / `IfcBuilding` / …) can
        // own Psets and Qtos. Most notably `Pset_SpaceCommon` and
        // `Qto_SpaceBaseQuantities` are normally attached to
        // `IfcSpace` instances. Resolve the property owner against
        // both the element table and the spatial table so neither
        // side is silently dropped on export.
        for (el, props) in properties.iter() {
            let Some(&owner_step) = element_step.get(el).or_else(|| spatial_step.get(el)) else {
                continue;
            };
            for (name, pset) in &props.psets {
                let prop_steps: Vec<u32> = pset
                    .properties
                    .iter()
                    .map(|(key, val)| {
                        let pid = buf.alloc();
                        let (lit, measure) = serialize_property_value(val);
                        buf.write_line(
                            pid,
                            format!(
                                "IFCPROPERTYSINGLEVALUE('{key}',$,{measure}({lit}),$)",
                                key = escape_step_string(key),
                                measure = measure,
                                lit = lit,
                            ),
                        );
                        pid
                    })
                    .collect();
                let pset_step = buf.alloc();
                let pset_guid = derive_guid_from_str(&format!("pset::{el}::{name}"));
                buf.write_line(
                    pset_step,
                    format!(
                        "IFCPROPERTYSET('{pset_guid}',#{owner},'{name}',$,({props}))",
                        owner = owner_history,
                        name = escape_step_string(name),
                        props = prop_steps
                            .iter()
                            .map(|p| format!("#{p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                );
                let rel = buf.alloc();
                let rel_guid = derive_guid_from_str(&format!("rel-pset::{el}::{name}"));
                buf.write_line(
                    rel,
                    format!(
                        "IFCRELDEFINESBYPROPERTIES('{rel_guid}',#{owner},$,$,(#{element}),#{pset})",
                        owner = owner_history,
                        element = owner_step,
                        pset = pset_step,
                    ),
                );
            }
            for (name, qset) in &props.qsets {
                let qty_steps: Vec<u32> = qset
                    .quantities
                    .iter()
                    .map(|(key, val)| {
                        let qid = buf.alloc();
                        let (lit, tag) = serialize_quantity_value(val);
                        buf.write_line(
                            qid,
                            format!(
                                "{tag}('{key}',$,$,{lit})",
                                key = escape_step_string(key),
                                lit = lit
                            ),
                        );
                        qid
                    })
                    .collect();
                let qset_step = buf.alloc();
                let qset_guid = derive_guid_from_str(&format!("qset::{el}::{name}"));
                buf.write_line(
                    qset_step,
                    format!(
                        "IFCELEMENTQUANTITY('{qset_guid}',#{owner},'{name}',$,$,({qts}))",
                        owner = owner_history,
                        name = escape_step_string(name),
                        qts = qty_steps
                            .iter()
                            .map(|p| format!("#{p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                );
                let rel = buf.alloc();
                let rel_guid = derive_guid_from_str(&format!("rel-qset::{el}::{name}"));
                buf.write_line(
                    rel,
                    format!(
                        "IFCRELDEFINESBYPROPERTIES('{rel_guid}',#{owner},$,$,(#{element}),#{qset})",
                        owner = owner_history,
                        element = owner_step,
                        qset = qset_step,
                    ),
                );
            }
        }

        // ---- Material library + IFCRELASSOCIATESMATERIAL ----
        //
        // Skip the whole section when the store is empty so the 3-arg
        // `to_string` path produces byte-identical output to the
        // pre-materials writer (and existing golden / regression tests
        // stay passing).
        if !materials.is_empty() {
            // Emit each `IfcMaterial` first so layer-set layers and
            // direct assignments can reference them.
            let mut material_step: HashMap<String, u32> = HashMap::new();
            for (name, mat) in materials.materials() {
                let step = buf.alloc();
                material_step.insert(name.clone(), step);
                let desc = step_optional_quoted(mat.description.as_deref());
                let cat = step_optional_quoted(mat.category.as_deref());
                buf.write_line(
                    step,
                    format!(
                        "IFCMATERIAL('{name}',{desc},{cat})",
                        name = escape_step_string(name),
                    ),
                );
            }
            // For each layer-set, emit all layers, then the set.
            // The same `Material` can be referenced from multiple
            // layers — the writer simply reuses the step id from the
            // `material_step` map. A layer whose `material_name`
            // doesn't resolve in the store is silently skipped
            // (matches the reader's tolerate-and-skip discipline).
            let mut layer_set_step: HashMap<String, u32> = HashMap::new();
            for (set_name, set) in materials.layer_sets() {
                let mut layer_step_ids: Vec<u32> = Vec::with_capacity(set.layers.len());
                for layer in &set.layers {
                    let Some(mat_step) = material_step.get(&layer.material_name) else {
                        continue;
                    };
                    let lid = buf.alloc();
                    layer_step_ids.push(lid);
                    let vent = match layer.is_ventilated {
                        Some(true) => ".T.",
                        Some(false) => ".F.",
                        None => ".U.",
                    };
                    let name_lit = step_optional_quoted(layer.name.as_deref());
                    let desc_lit = step_optional_quoted(layer.description.as_deref());
                    let cat_lit = step_optional_quoted(layer.category.as_deref());
                    let prio_lit = layer
                        .priority
                        .map_or_else(|| "$".to_string(), |p| p.to_string());
                    buf.write_line(
                        lid,
                        format!(
                            "IFCMATERIALLAYER(#{mat},{thickness},{vent},{name_lit},{desc_lit},{cat_lit},{prio_lit})",
                            mat = mat_step,
                            thickness = format_real(layer.thickness_m),
                        ),
                    );
                }
                // IFC4 `IfcMaterialLayerSet.MaterialLayers` is
                // `LIST [1:?]` — emitting a set with zero resolved
                // layers would produce a schema-invalid `(())` literal.
                // Drop the set entirely: any `MaterialAssignment::LayerSet`
                // bound to this name will then cascade into the
                // tolerate-and-skip path below.
                if layer_step_ids.is_empty() {
                    continue;
                }
                let sid = buf.alloc();
                layer_set_step.insert(set_name.clone(), sid);
                let desc_lit = step_optional_quoted(set.description.as_deref());
                buf.write_line(
                    sid,
                    format!(
                        "IFCMATERIALLAYERSET(({layers}),'{name}',{desc_lit})",
                        layers = layer_step_ids
                            .iter()
                            .map(|p| format!("#{p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                        name = escape_step_string(set_name),
                    ),
                );
            }
            // Group element ↔ material bindings by the material /
            // layer-set side so we emit one `IfcRelAssociatesMaterial`
            // per material with a list of associated elements (rather
            // than one rel per element). This mirrors how authoring
            // tools (Revit, ArchiCAD) emit the relation and keeps the
            // STEP file small.
            //
            // BTreeMap so iteration is deterministic (alphabetical
            // by material name) — matters for the byte-identical
            // round-trip guarantee.
            let mut by_material: BTreeMap<(bool, String), Vec<u32>> = BTreeMap::new();
            for (entity, assignment) in materials.assignments() {
                // Per IFC4 schema, `IfcRelAssociatesMaterial.RelatedObjects`
                // is `SET[1:?] OF IfcObjectDefinition` — same supertype
                // that `IfcRelDefinesByProperties.RelatedObjects` uses
                // (see Pset path at line ~336). Both element subtypes
                // and spatial-structure subtypes (`IfcSpace`,
                // `IfcBuildingStorey`, …) can carry a material binding;
                // notably a `Pset_SpaceCommon`-tagged `IfcSpace` can
                // own an `IfcMaterial` for the dominant floor finish.
                // The reader already accepts both element_step and
                // spatial_step targets — the writer must mirror that or
                // round-tripping a Revit / ArchiCAD-authored file
                // silently drops space ↔ material bindings.
                let Some(elem_step) = element_step
                    .get(entity)
                    .or_else(|| spatial_step.get(entity))
                else {
                    continue;
                };
                let key = match assignment {
                    MaterialAssignment::Single(name) => (false, name.clone()),
                    MaterialAssignment::LayerSet(name) => (true, name.clone()),
                };
                by_material.entry(key).or_default().push(*elem_step);
            }
            for ((is_layer_set, name), elem_steps) in by_material {
                let mat_ref = if is_layer_set {
                    layer_set_step.get(&name).copied()
                } else {
                    material_step.get(&name).copied()
                };
                let Some(mat_ref) = mat_ref else {
                    continue;
                };
                let rel = buf.alloc();
                let rel_guid = derive_guid_from_str(&format!(
                    "rel-mat::{kind}::{name}",
                    kind = if is_layer_set { "set" } else { "single" },
                ));
                buf.write_line(
                    rel,
                    format!(
                        "IFCRELASSOCIATESMATERIAL('{rel_guid}',#{owner},$,$,({elems}),#{mat})",
                        owner = owner_history,
                        elems = elem_steps
                            .iter()
                            .map(|p| format!("#{p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                        mat = mat_ref,
                    ),
                );
            }
        }

        buf.inner.extend_from_slice(b"ENDSEC;\nEND-ISO-10303-21;\n");
        String::from_utf8(buf.inner).expect("IFC writer emits ASCII only")
    }

    /// Stream the project + classification + property graph to a
    /// `Write` sink. Materials are omitted — see
    /// [`IfcWriter::write_with_materials`] for the 5-arg streaming
    /// counterpart.
    ///
    /// The 4-arg signature stays as-is for existing callers (asset
    /// export, journey tests). New code that needs material data in
    /// the output should use `write_with_materials`.
    pub fn write<W: Write>(
        w: &mut W,
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
    ) -> Result<(), IfcWriteError> {
        Self::write_with_materials(
            w,
            project,
            classification,
            properties,
            &MaterialStore::default(),
        )
    }

    /// Stream the project + classification + property + material
    /// library to a `Write` sink. Symmetric to
    /// [`IfcWriter::to_string_with_materials`] but emits to a
    /// `Write` sink rather than materialising the full STEP body in
    /// memory.
    pub fn write_with_materials<W: Write>(
        w: &mut W,
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
        materials: &MaterialStore,
    ) -> Result<(), IfcWriteError> {
        let s = Self::to_string_with_materials(project, classification, properties, materials);
        w.write_all(s.as_bytes())?;
        Ok(())
    }
}

/// Returns true when `s` is a valid STEP-21 SIMPLE_ID safe to embed as
/// the entity-type prefix of a STEP record.
///
/// Per ISO 10303-21 a SIMPLE_ID matches `[A-Za-z_][A-Za-z0-9_]*`. The
/// AEC Studio in-process round-trip additionally accepts `:` so that
/// `IfcClass::Other("Some::Custom::Type")` produced by an extension
/// classifier can be emitted verbatim (`:` does not collide with any
/// STEP delimiter and the reader splits the Name field with
/// `rsplit_once("::")`). Characters that *would* corrupt
/// `parse_step_groups` in the reader — `(`, `)`, `'`, `,`, `;`,
/// whitespace, control chars — are rejected so the writer can fall
/// back to a canonical proxy type instead of emitting a malformed line.
pub(crate) fn is_step_safe_simple_id(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Returns true when `class` is one of the IFC4 *spatial structure*
/// classes (Project / Site / Building / Storey / Space) that the
/// writer emits as a top-level `SpatialRow` and that the reader's
/// first-pass dispatch routes into the spatial-node branch (rather
/// than the element branch that decodes `{tag}::{eid}` from the Name
/// field).
///
/// If an element — i.e. something attached via
/// `Project::attach_element` — were emitted with a spatial entity
/// type, the reader would silently re-classify it as a spatial node
/// with a fresh EntityId and the original element identity would be
/// lost on round-trip. Currently no code in the crate does this, but
/// the writer guards against the misuse defensively (same pattern as
/// the STEP-unsafe `Other(_)` tag fallback): if an element is
/// classified as one of these spatial classes, the writer emits the
/// STEP type as `IfcBuildingElementProxy` and keeps the original tag
/// in the Name field, so the reader's name-prefers-tag path
/// reconstructs the class losslessly.
pub(crate) fn is_spatial_ifc_class(class: &IfcClass) -> bool {
    matches!(
        class,
        IfcClass::IfcProject
            | IfcClass::IfcSite
            | IfcClass::IfcBuilding
            | IfcClass::IfcBuildingStorey
            | IfcClass::IfcSpace
    )
}

/// Escape `s` for embedding inside a STEP single-quoted string.
///
/// The on-wire format produced by the writer is:
///
///   * `\` is escaped as `\\`
///   * `'` is escaped as `''` (ISO 10303-21 §6.4.1 canonical doubled
///     single-quote — interoperable with Revit, ArchiCAD, IfcOpenShell,
///     and any conforming STEP toolchain)
///   * `\n` (U+000A) is escaped as `\n` (backslash + ASCII 'n')
///   * `\r` (U+000D) is escaped as `\r` (backslash + ASCII 'r')
///   * `\t` (U+0009) is escaped as `\t` (backslash + ASCII 't')
///
/// The reader ([`super::reader::unescape_step_string`]) accepts BOTH
/// the canonical `''` form emitted here AND the legacy `\'` form
/// emitted by pre-Phase-9 AEC builds, so files already on disk
/// continue to round-trip cleanly after this writer change. The
/// shared [`super::reader::StepRecordSplitter`] state machine
/// likewise treats both `''` and `\'` as non-terminating inside a
/// string literal.
///
/// The newline / carriage-return / tab escapes are an
/// interop-friendly invariant: the writer emits exactly one
/// `#N = TYPE(...);` record per physical line so the output is
/// readable by tools that line-tokenise STEP (older IfcOpenShell
/// versions, naive grep / awk pipelines). Strictly speaking the
/// reader's character-level state machine would handle embedded
/// newlines correctly, but downstream consumers might not, so we
/// keep them escaped at the write side.
///
/// Order matters: backslashes must be doubled BEFORE the other
/// substitutions, otherwise the `\` introduced for an escaped
/// newline would itself be doubled and corrupt the encoding. The
/// `match` arm on `'\\'` runs first by construction. The doubled-
/// quote substitution does not introduce any `\` so it can run in
/// any position relative to the backslash arm.
///
/// Control characters outside the small ASCII subset above
/// (`\X\` / `\X2\` / `\X4\` Page-1 / Page-2 / Page-4 encodings)
/// are not produced by the writer — input strings come from
/// IfcLabel / IfcText fields which the rest of the engine treats as
/// UTF-8 and which the reader passes through verbatim.
fn escape_step_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("''"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Returns the (literal, measure-wrapper) pair for a single property
/// value. The measure-wrapper is the IFC tag uppercased to STEP
/// canonical form (e.g. `"IFCMASSDENSITYMEASURE"`). For unmodeled
/// `PropertyValue::Other` variants the raw STEP literal is emitted
/// verbatim, restoring the bytes the reader captured.
fn serialize_property_value(v: &PropertyValue) -> (String, String) {
    match v {
        PropertyValue::Text(s) => (
            format!("'{}'", escape_step_string(s)),
            "IFCTEXT".to_string(),
        ),
        PropertyValue::Label(s) => (
            format!("'{}'", escape_step_string(s)),
            "IFCLABEL".to_string(),
        ),
        PropertyValue::Real(x) => (format_real(*x), "IFCREAL".to_string()),
        PropertyValue::Length(x) => (format_real(*x), "IFCLENGTHMEASURE".to_string()),
        PropertyValue::Area(x) => (format_real(*x), "IFCAREAMEASURE".to_string()),
        PropertyValue::Volume(x) => (format_real(*x), "IFCVOLUMEMEASURE".to_string()),
        PropertyValue::Ratio(x) => (format_real(*x), "IFCPOSITIVERATIOMEASURE".to_string()),
        PropertyValue::Integer(i) => (i.to_string(), "IFCINTEGER".to_string()),
        PropertyValue::Boolean(b) => (
            if *b { ".T." } else { ".F." }.to_string(),
            "IFCBOOLEAN".to_string(),
        ),
        PropertyValue::Other { measure, raw } => (raw.clone(), measure.to_ascii_uppercase()),
    }
}

/// Serialise a `PropertyValue` as an `IfcQuantity*` STEP literal for
/// inclusion in an `IfcElementQuantity` set.
///
/// **Contract for `PropertyValue::Other`**: when an `Other` variant is
/// placed in a `QuantitySet`, its `measure` field MUST already be an
/// `IFCQUANTITY*` entity type (e.g. `"IFCQUANTITYTIME"` for the
/// IFC4x3 time quantity, or any future schema-level quantity). The
/// reader enforces this by routing only entities matching the
/// `IFCQUANTITY*` prefix through `parse_quantity_typed` → the
/// catch-all `Other` arm (see `crates/aec_bim/src/ifc/reader.rs` ::
/// `parse_quantity_typed`). Programmatic callers stuffing an `Other`
/// variant with a non-quantity measure (e.g. `IFCMASSDENSITYMEASURE`)
/// into a QuantitySet would produce malformed STEP — the
/// `debug_assert!` below catches that contract violation in tests and
/// debug builds while staying free in release. The check is a
/// fail-fast on programmer error, not a runtime tax on well-formed
/// input.
fn serialize_quantity_value(v: &PropertyValue) -> (String, String) {
    match v {
        PropertyValue::Length(x) => (format_real(*x), "IFCQUANTITYLENGTH".to_string()),
        PropertyValue::Area(x) => (format_real(*x), "IFCQUANTITYAREA".to_string()),
        PropertyValue::Volume(x) => (format_real(*x), "IFCQUANTITYVOLUME".to_string()),
        PropertyValue::Integer(i) => (i.to_string(), "IFCQUANTITYCOUNT".to_string()),
        PropertyValue::Real(x) => (format_real(*x), "IFCQUANTITYWEIGHT".to_string()),
        PropertyValue::Other { measure, raw } => {
            let entity = measure.to_ascii_uppercase();
            debug_assert!(
                entity.starts_with("IFCQUANTITY"),
                "PropertyValue::Other in a QuantitySet must carry an IFCQUANTITY* \
                 measure (e.g. IFCQUANTITYTIME for IFC4x3); got {entity:?}. \
                 The reader only routes IFCQUANTITY* prefixed entities into the \
                 Other arm of parse_quantity_typed, so this assertion firing means \
                 a programmatic caller bypassed the reader and stuffed a non-\
                 quantity measure into a QuantitySet — it would produce \
                 malformed STEP. Move the value into a PropertySet instead, \
                 where any IfcMeasure can be serialised."
            );
            (raw.clone(), entity)
        }
        PropertyValue::Ratio(_) => {
            // IFC4 has no IfcQuantityRatio — Ratio values belong in
            // Psets (IFCPOSITIVERATIOMEASURE) not Qsets. Emit as
            // IFCQUANTITYWEIGHT so the file stays parseable, but flag
            // this at dev-time so the authoring code can be corrected.
            debug_assert!(
                false,
                "PropertyValue::Ratio in a QuantitySet has no lossless IFC4 \
                 representation; move it to a PropertySet where \
                 IFCPOSITIVERATIOMEASURE preserves type fidelity"
            );
            (
                format_real(match v {
                    PropertyValue::Ratio(x) => *x,
                    _ => 0.0,
                }),
                "IFCQUANTITYWEIGHT".to_string(),
            )
        }
        // Boolean / text quantities aren't standard IFC; fall through
        // as IfcQuantityCount(0) so the file still parses.
        _ => ("0".into(), "IFCQUANTITYCOUNT".to_string()),
    }
}

/// Render an optional UTF-8 string as a STEP literal. `None` →
/// `"$"` (the IFC sentinel for "value omitted"); `Some(s)` →
/// `"'…escaped…'"` so embedded quotes / backslashes stay
/// well-formed.
fn step_optional_quoted(s: Option<&str>) -> String {
    s.map_or_else(
        || "$".to_string(),
        |t| format!("'{}'", escape_step_string(t)),
    )
}

fn format_real(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1.0e15 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}
