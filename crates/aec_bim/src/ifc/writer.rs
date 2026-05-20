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

use std::collections::HashMap;
use std::io::Write;

use thiserror::Error;

use aec_core::types::EntityId;

use crate::classification::{ClassificationStore, IfcClass};
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
    pub fn to_string(
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
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
        let mut spatial_step: HashMap<EntityId, u32> = HashMap::new();
        let mut spatial_guid: HashMap<EntityId, String> = HashMap::new();
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
            spatial_guid.insert(id.clone(), guid.clone());
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
        for (id, node) in &project.nodes {
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
        let mut element_step: HashMap<EntityId, u32> = HashMap::new();
        let mut element_guid: HashMap<EntityId, String> = HashMap::new();
        for (storey_id, node) in &project.nodes {
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
                element_guid.insert(el.clone(), guid.clone());
                let tag = class.ifc_tag();
                buf.write_line(
                    step_id,
                    format!(
                        "{tag}('{guid}',#{owner},$,'{name}',$,$,$,$,$)",
                        owner = owner_history,
                        // Defense in depth: even though `tag` and the
                        // EntityId Display are ASCII-safe today, route
                        // the composed name through `escape_step_string`
                        // so adding any future user-supplied component
                        // can't break STEP tokenization.
                        name = escape_step_string(&format!("{tag}::{}", el.to_string())),
                    ),
                );
                contained_refs.push(format!("#{step_id}"));
            }
            if !contained_refs.is_empty() {
                let rel = buf.alloc();
                let rel_guid =
                    derive_guid_from_str(&format!("rel-contained::{}", storey_id.to_string()));
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
        for (el, props) in properties.iter() {
            let Some(&owner_step) = element_step.get(el) else {
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
                let pset_guid =
                    derive_guid_from_str(&format!("pset::{}::{}", el.to_string(), name));
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
                let rel_guid =
                    derive_guid_from_str(&format!("rel-pset::{}::{}", el.to_string(), name));
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
                let qset_guid =
                    derive_guid_from_str(&format!("qset::{}::{}", el.to_string(), name));
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
                let rel_guid =
                    derive_guid_from_str(&format!("rel-qset::{}::{}", el.to_string(), name));
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

        buf.inner.extend_from_slice(b"ENDSEC;\nEND-ISO-10303-21;\n");
        String::from_utf8(buf.inner).expect("IFC writer emits ASCII only")
    }

    pub fn write<W: Write>(
        w: &mut W,
        project: &Project,
        classification: &ClassificationStore,
        properties: &PropertyStore,
    ) -> Result<(), IfcWriteError> {
        let s = Self::to_string(project, classification, properties);
        w.write_all(s.as_bytes())?;
        Ok(())
    }
}

/// Escape `s` for embedding inside a STEP single-quoted string.
///
/// The on-wire format used by the writer/reader pair is:
///
///   * `\` is escaped as `\\`
///   * `'` is escaped as `\'`
///   * `\n` (U+000A) is escaped as `\n` (backslash + ASCII 'n')
///   * `\r` (U+000D) is escaped as `\r` (backslash + ASCII 'r')
///   * `\t` (U+0009) is escaped as `\t` (backslash + ASCII 't')
///
/// The newline / carriage-return / tab escapes are critical for
/// correctness: the reader tokenises the STEP byte stream with
/// [`str::lines`] under the assumption that one `#N = TYPE(...);`
/// record fits on a single line. A literal newline in a user-supplied
/// name (e.g. a multi-line space description copy-pasted by an
/// operator) would otherwise split the record across two lines and
/// surface as an opaque `IfcReadError::Malformed` on re-import.
///
/// Order matters: backslashes must be doubled BEFORE the other
/// substitutions, otherwise the `\` introduced for an escaped quote
/// or newline would itself be doubled and corrupt the encoding. The
/// `match` arm on `'\\'` runs first by construction.
///
/// This is not ISO 10303-21 canonical (canonical STEP doubles single
/// quotes as `''` and uses `\X\` / `\X2\` / `\X4\` for control
/// characters), but matches the reader's single-pass unescape in
/// [`unescape_step_string`]. The module docs explicitly state this
/// reader/writer pair is for AEC Studio's in-process IFC pipeline and
/// not intended for interop with third-party IFC tooling.
fn escape_step_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn serialize_property_value(v: &PropertyValue) -> (String, &'static str) {
    match v {
        PropertyValue::Text(s) => (format!("'{}'", escape_step_string(s)), "IFCTEXT"),
        PropertyValue::Label(s) => (format!("'{}'", escape_step_string(s)), "IFCLABEL"),
        PropertyValue::Real(x) => (format_real(*x), "IFCREAL"),
        PropertyValue::Length(x) => (format_real(*x), "IFCLENGTHMEASURE"),
        PropertyValue::Area(x) => (format_real(*x), "IFCAREAMEASURE"),
        PropertyValue::Volume(x) => (format_real(*x), "IFCVOLUMEMEASURE"),
        PropertyValue::Ratio(x) => (format_real(*x), "IFCPOSITIVERATIOMEASURE"),
        PropertyValue::Integer(i) => (i.to_string(), "IFCINTEGER"),
        PropertyValue::Boolean(b) => (if *b { ".T." } else { ".F." }.to_string(), "IFCBOOLEAN"),
    }
}

fn serialize_quantity_value(v: &PropertyValue) -> (String, &'static str) {
    match v {
        PropertyValue::Length(x) => (format_real(*x), "IFCQUANTITYLENGTH"),
        PropertyValue::Area(x) => (format_real(*x), "IFCQUANTITYAREA"),
        PropertyValue::Volume(x) => (format_real(*x), "IFCQUANTITYVOLUME"),
        PropertyValue::Integer(i) => (i.to_string(), "IFCQUANTITYCOUNT"),
        PropertyValue::Real(x) => (format_real(*x), "IFCQUANTITYWEIGHT"),
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
                "IFCQUANTITYWEIGHT",
            )
        }
        // Boolean / text quantities aren't standard IFC; fall through
        // as IfcQuantityCount(0) so the file still parses.
        _ => ("0".into(), "IFCQUANTITYCOUNT"),
    }
}

fn format_real(x: f64) -> String {
    if x == x.trunc() && x.abs() < 1.0e15 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}
