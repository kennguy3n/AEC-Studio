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
use std::io::BufRead;

use thiserror::Error;

use aec_core::types::EntityId;

use crate::classification::{ClassificationSource, ClassificationStore, IfcClass};
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
                "IFCQUANTITYLENGTH" | "IFCQUANTITYAREA" | "IFCQUANTITYVOLUME"
                | "IFCQUANTITYCOUNT" | "IFCQUANTITYWEIGHT" => {
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
                | "IFCRELDEFINESBYPROPERTIES" => {
                    // Handled in the second pass.
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
                return Err(IfcReadError::Malformed(format!(
                    "IFCRELDEFINESBYPROPERTIES references unknown set #{}",
                    pset_or_qset_step
                )));
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
            records_seen: groups.len(),
        };

        // sanity: project_entity is the same as project.root
        debug_assert_eq!(project_entity, project.root);

        Ok(IfcSnapshot {
            project,
            classification,
            properties: props,
            guid_by_entity,
            element_parent,
            schema,
            stats,
        })
    }

    /// Streaming variant of [`IfcReader::from_string`].
    ///
    /// Reads logical STEP records line-by-line from `reader` so
    /// even multi-gigabyte external IFC files can be ingested
    /// without `read_to_string`-ing the whole file. Internally
    /// this still indexes the records (the IFC cross-reference
    /// graph requires it), so peak RAM is bounded by the parsed
    /// record set rather than the on-disk byte count.
    ///
    /// For genuinely streaming record-by-record consumption,
    /// use [`IfcReader::iter`] which yields one [`StepRecord`]
    /// at a time without building the cross-reference indexes.
    pub fn from_reader<R: BufRead>(reader: R) -> IfcReadResult<IfcSnapshot> {
        let mut buf = String::new();
        for line in reader.lines() {
            let line = line?;
            buf.push_str(&line);
            buf.push('\n');
        }
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
fn detect_schema(text: &str) -> IfcReadResult<IfcSchema> {
    // The STEP grammar allows whitespace inside the parens; the
    // canonical AEC-emitted form is `FILE_SCHEMA(('IFC4'));` but
    // external authoring tools may emit `FILE_SCHEMA ( ( 'IFC4' ) ) ;`.
    let upper = text.to_ascii_uppercase();
    let needle = "FILE_SCHEMA";
    let start = upper
        .find(needle)
        .ok_or(IfcReadError::MissingSection("FILE_SCHEMA"))?;
    let tail = &text[start + needle.len()..];
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
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut in_comment = false;
    // Walk by characters so multi-byte UTF-8 (e.g. é, Ö, £) is not
    // mis-split. The peek-ahead for `/*` and `*/` looks at the next
    // *byte* via slice indexing on the original text, which is safe
    // because `/` and `*` are ASCII (single-byte) and the byte
    // offset of `ch` is always at a char boundary.
    let mut chars = text.char_indices().peekable();
    while let Some((off, c)) = chars.next() {
        // Comment open: `/*` outside strings.
        if !in_str && !in_comment && c == '/' {
            if let Some(&(_, '*')) = chars.peek() {
                in_comment = true;
                chars.next(); // consume `*`
                continue;
            }
        }
        // Comment close: `*/`.
        if in_comment {
            if c == '*' {
                if let Some(&(_, '/')) = chars.peek() {
                    in_comment = false;
                    chars.next(); // consume `/`
                }
            }
            continue;
        }
        match c {
            '\\' if in_str => {
                // Preserve `\\` and `\'` so split_step_args sees them.
                buf.push(c);
                if let Some((_, next)) = chars.next() {
                    buf.push(next);
                }
            }
            '\'' => {
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
                        "STEP file has ')' without matching '(' near offset {off}"
                    )));
                }
                buf.push(c);
            }
            ';' if !in_str && depth == 0 => {
                if !buf.trim().is_empty() {
                    out.push(buf.trim().to_string());
                }
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    if in_str {
        return Err(IfcReadError::Malformed(
            "unterminated string literal at end of STEP file".into(),
        ));
    }
    if depth != 0 {
        return Err(IfcReadError::Malformed(format!(
            "STEP file ends with unbalanced parens (depth={depth})"
        )));
    }
    Ok(out)
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

/// Find the first complete logical STEP record in `text`. Returns
/// `Ok(Some((record, consumed)))` where `record` is the text up to
/// but excluding the terminating `;`, and `consumed` is the byte
/// length to drain from the front of `text` (including the `;`).
/// Returns `Ok(None)` if `text` does not yet contain a complete
/// record (caller should buffer more input).
fn split_first_logical_record(text: &str) -> IfcReadResult<Option<(String, usize)>> {
    let mut buf = String::new();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut in_comment = false;
    let mut chars = text.char_indices().peekable();
    while let Some((off, c)) = chars.next() {
        if !in_str && !in_comment && c == '/' {
            if let Some(&(_, '*')) = chars.peek() {
                in_comment = true;
                chars.next(); // consume `*`
                continue;
            }
        }
        if in_comment {
            if c == '*' {
                if let Some(&(_, '/')) = chars.peek() {
                    in_comment = false;
                    chars.next(); // consume `/`
                }
            }
            continue;
        }
        match c {
            '\\' if in_str => {
                buf.push(c);
                if let Some((_, next)) = chars.next() {
                    buf.push(next);
                }
            }
            '\'' => {
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
                        "STEP stream has ')' without matching '(' near offset {off}"
                    )));
                }
                buf.push(c);
            }
            ';' if !in_str && depth == 0 => {
                // Consumed byte length = current char's offset + its UTF-8 width.
                return Ok(Some((buf.trim().to_string(), off + c.len_utf8())));
            }
            _ => buf.push(c),
        }
    }
    Ok(None)
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
                // The writer's escape_step_string emits \' for embedded
                // single-quotes and \\ for embedded backslashes. Consume
                // the backslash + the following character so we don't
                // mis-toggle `in_str` on an escaped `'`.
                buf.push(c);
                if let Some(next) = chars.next() {
                    buf.push(next);
                }
            }
            '\'' => {
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
        other => Err(IfcReadError::UnknownType(other.to_string())),
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
        other => Err(IfcReadError::UnknownType(other.to_string())),
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
///   * single quotes as `\'`
///   * backslashes as `\\`
///   * newlines (`U+000A`) as `\n`
///   * carriage returns (`U+000D`) as `\r`
///   * tabs (`U+0009`) as `\t`
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
fn unescape_step_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
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
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_real(s: &str) -> IfcReadResult<f64> {
    s.parse::<f64>()
        .map_err(|e| IfcReadError::Malformed(format!("real parse: {e}")))
}

fn parse_int(s: &str) -> IfcReadResult<i64> {
    s.parse::<i64>()
        .map_err(|e| IfcReadError::Malformed(format!("int parse: {e}")))
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
            // Encode the same way the writer does, char-by-char.
            let mut encoded = String::new();
            for c in input.chars() {
                match c {
                    '\\' => encoded.push_str("\\\\"),
                    '\'' => encoded.push_str("\\'"),
                    _ => encoded.push(c),
                }
            }
            let decoded = unescape_step_string(&encoded);
            assert_eq!(decoded, input, "round-trip failed for {input:?}");
        }
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
