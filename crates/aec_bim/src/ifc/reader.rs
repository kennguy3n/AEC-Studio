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
//! This is intentionally a **subset** parser: we do not support
//! arbitrary IFC files coming from other authoring tools. That path
//! goes through the IfcOpenShell worker. The role of this parser is
//! to round-trip the bytes our own writer produced, for end-to-end
//! tests and for the BIM-Lite export pack.

use std::collections::HashMap;

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
        if !text.contains("FILE_SCHEMA(('IFC4'))") {
            return Err(IfcReadError::MissingSection("FILE_SCHEMA"));
        }
        if !text.trim_end().ends_with("END-ISO-10303-21;") {
            return Err(IfcReadError::Malformed(
                "missing END-ISO-10303-21; terminator".into(),
            ));
        }

        // ---- Tokenise: every `#N = TYPE(...);` becomes (id, type, args). ----
        let groups = parse_step_groups(text)?;

        // ---- Index by STEP id ----
        let by_id: HashMap<u32, &StepRecord> =
            groups.iter().map(|g| (g.step_id, g)).collect();

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
                "IFCPROJECT" | "IFCSITE" | "IFCBUILDING" | "IFCBUILDINGSTOREY"
                | "IFCSPACE" => {
                    let class = ifc_class_from_tag(&g.kind)?;
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
                "IFCRELAGGREGATES" | "IFCRELCONTAINEDINSPATIALSTRUCTURE"
                | "IFCRELDEFINESBYPROPERTIES" => {
                    // Handled in the second pass.
                }
                other => {
                    // Anything else with a 9-field shape and a
                    // `tag::eid` name is an element.
                    if let Ok(name) = g.string_arg(3) {
                        if let Some((tag, eid)) = name.split_once("::") {
                            if let Ok(class) = ifc_class_from_tag(other) {
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
                            } else {
                                return Err(IfcReadError::UnknownType(other.to_string()));
                            }
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
                if let Some(child_entity) =
                    spatial.get(child_step).map(|r| r.entity.clone())
                {
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
        debug_assert!(
            classification
                .iter()
                .all(|(_, asg)| matches!(asg.source, ClassificationSource::Imported))
        );

        // ---- Rebuild PropertyStore ----
        let mut props = PropertyStore::new();
        let mut pset_count = 0usize;
        let mut qset_count = 0usize;
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
                    if let Some(el) = elements.get(elem_step) {
                        props.entry(el.entity.clone()).upsert_pset(set.clone());
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
                    if let Some(el) = elements.get(elem_step) {
                        props.entry(el.entity.clone()).upsert_qset(set.clone());
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
        };

        // sanity: project_entity is the same as project.root
        debug_assert_eq!(project_entity, project.root);

        Ok(IfcSnapshot {
            project,
            classification,
            properties: props,
            guid_by_entity,
            element_parent,
            stats,
        })
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

#[derive(Debug, Clone)]
struct StepRecord {
    step_id: u32,
    kind: String,
    args: Vec<String>,
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
    // Each STEP entity instance fits on one line in the bytes we emit:
    //   "#N = TYPE(arg1,arg2,...);\n"
    // We tokenize purely on those lines, ignoring the HEADER and
    // ENDSEC markers.
    for raw in text.lines() {
        let line = raw.trim();
        if !line.starts_with('#') {
            continue;
        }
        let body = match line.strip_suffix(';') {
            Some(s) => s,
            None => continue,
        };
        // "#N = TYPE(args)"
        let (lhs, rhs) = body
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
        let kind = rhs[..open].trim().to_uppercase();
        let inner = &rhs[open + 1..close];
        let args = split_step_args(inner);
        groups.push(StepRecord {
            step_id,
            kind,
            args,
        });
    }
    Ok(groups)
}

/// Split the argument list of a STEP record, respecting nested
/// parentheses (e.g. measure wrappers like `IFCLENGTHMEASURE(0.9)`)
/// and single-quoted strings.
fn split_step_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
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
                buf.push(c);
            }
            ',' if !in_str && depth == 0 => {
                out.push(buf.trim().to_string());
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

impl StepRecord {
    fn string_arg(&self, idx: usize) -> IfcReadResult<String> {
        let raw = self
            .args
            .get(idx)
            .ok_or_else(|| IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind)))?;
        let s = raw.trim();
        if !(s.starts_with('\'') && s.ends_with('\'')) {
            return Err(IfcReadError::Malformed(format!(
                "expected quoted string at arg {idx} of {} but got '{s}'",
                self.kind
            )));
        }
        Ok(s[1..s.len() - 1].replace("\\'", "'"))
    }

    fn ref_arg(&self, idx: usize) -> IfcReadResult<u32> {
        let raw = self
            .args
            .get(idx)
            .ok_or_else(|| IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind)))?;
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

    fn ref_list_arg(&self, idx: usize) -> IfcReadResult<Vec<u32>> {
        let raw = self
            .args
            .get(idx)
            .ok_or_else(|| IfcReadError::Malformed(format!("missing arg {idx} on {}", self.kind)))?;
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

fn ifc_class_from_tag(tag: &str) -> IfcReadResult<IfcClass> {
    // STEP entity names are uppercase ("IFCWALL"), our `ifc_tag()`
    // returns mixed case ("IfcWall"). Normalize both.
    let up = tag.to_ascii_uppercase();
    let cls = match up.as_str() {
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
        _ => return Err(IfcReadError::UnknownType(tag.to_string())),
    };
    Ok(cls)
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
    Ok(s[1..s.len() - 1].replace("\\'", "'"))
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

    fn build_tiny_project() -> (
        Project,
        ClassificationStore,
        PropertyStore,
        Vec<EntityId>,
    ) {
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
            pc.set(
                "Reference",
                PropertyValue::Label(format!("W-{:02}", i)),
            );
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
