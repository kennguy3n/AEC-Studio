//! Rule-based validator for BIM projects.
//!
//! Each `validate_*` function returns a list of [`ValidationFinding`]s.
//! `validate_project` runs every check.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::classification::ClassificationStore;
use crate::properties::{standard_pset_keys, standard_pset_name_for_class, PropertyStore};
use crate::relations::RelationStore;
use crate::spatial::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationFinding {
    pub severity: ValidationSeverity,
    pub code: String,
    pub element: Option<EntityId>,
    pub description: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub findings: Vec<ValidationFinding>,
}

impl ValidationReport {
    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Warning)
            .count()
    }
    pub fn is_clean(&self) -> bool {
        self.errors() == 0
    }
}

pub fn validate_project(
    project: &Project,
    classification: &ClassificationStore,
    props: &PropertyStore,
    relations: &RelationStore,
) -> ValidationReport {
    let mut out = Vec::new();
    out.extend(check_dangling_relations(project, classification, relations));
    out.extend(check_missing_classifications(project, classification));
    out.extend(check_duplicate_guids(project));
    out.extend(check_required_psets(classification, props));
    out.extend(check_orphan_elements(project, classification, relations));
    ValidationReport { findings: out }
}

/// Relation references to entities that don't exist in the project.
fn check_dangling_relations(
    project: &Project,
    classification: &ClassificationStore,
    relations: &RelationStore,
) -> Vec<ValidationFinding> {
    let mut known: HashSet<EntityId> = project.nodes.keys().cloned().collect();
    for (id, _) in classification.iter() {
        known.insert(id.clone());
    }
    let mut out = Vec::new();
    for rel in relations.aggregates() {
        if !known.contains(&rel.relating) {
            out.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                code: "BIM_DANGLING_AGGREGATE_PARENT".into(),
                element: Some(rel.relating.clone()),
                description: format!(
                    "IfcRelAggregates {} points to a relating entity that doesn't exist",
                    rel.guid
                ),
                suggestion: Some("Remove or repoint the relationship".into()),
            });
        }
        for child in &rel.related {
            if !known.contains(child) {
                out.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    code: "BIM_DANGLING_AGGREGATE_CHILD".into(),
                    element: Some(child.clone()),
                    description: format!(
                        "IfcRelAggregates {} references a child entity that doesn't exist",
                        rel.guid
                    ),
                    suggestion: None,
                });
            }
        }
    }
    for rel in relations.containments() {
        if !known.contains(&rel.spatial_node) {
            out.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                code: "BIM_DANGLING_CONTAINER".into(),
                element: Some(rel.spatial_node.clone()),
                description: format!(
                    "IfcRelContainedInSpatialStructure {} points to a missing container",
                    rel.guid
                ),
                suggestion: None,
            });
        }
        for el in &rel.elements {
            if !known.contains(el) {
                out.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    code: "BIM_DANGLING_CONTAINED".into(),
                    element: Some(el.clone()),
                    description: format!(
                        "IfcRelContainedInSpatialStructure {} references a missing element",
                        rel.guid
                    ),
                    suggestion: Some("Remove the reference or restore the element".into()),
                });
            }
        }
    }
    out
}

/// Spatial nodes (other than the root) that have geometry but no
/// classification.
fn check_missing_classifications(
    project: &Project,
    classification: &ClassificationStore,
) -> Vec<ValidationFinding> {
    let mut out = Vec::new();
    for (id, node) in &project.nodes {
        if id == &project.root {
            continue;
        }
        if node.elements.is_empty() {
            continue;
        }
        // Each attached element should have a classification entry.
        for el in &node.elements {
            if classification.get(el).is_none() {
                out.push(ValidationFinding {
                    severity: ValidationSeverity::Warning,
                    code: "BIM_MISSING_CLASSIFICATION".into(),
                    element: Some(el.clone()),
                    description: format!(
                        "Element attached to '{}' has no IFC classification",
                        node.name
                    ),
                    suggestion: Some(
                        "Classify the element manually or run the AI classifier".into(),
                    ),
                });
            }
        }
    }
    out
}

fn check_duplicate_guids(project: &Project) -> Vec<ValidationFinding> {
    let mut seen: std::collections::HashMap<String, EntityId> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for (id, node) in &project.nodes {
        if let Some(guid) = &node.ifc_guid {
            if let Some(prev) = seen.get(guid) {
                out.push(ValidationFinding {
                    severity: ValidationSeverity::Error,
                    code: "BIM_DUPLICATE_GUID".into(),
                    element: Some(id.clone()),
                    description: format!("GUID {} is shared with another entity {:?}", guid, prev),
                    suggestion: Some("Regenerate one of the two GUIDs".into()),
                });
            } else {
                seen.insert(guid.clone(), id.clone());
            }
        }
    }
    out
}

fn check_required_psets(
    classification: &ClassificationStore,
    props: &PropertyStore,
) -> Vec<ValidationFinding> {
    let mut out = Vec::new();
    for (id, asg) in classification.iter() {
        let class = &asg.class;
        if !class.is_building_element() {
            continue;
        }
        let Some(pset_name) = standard_pset_name_for_class(class) else {
            continue;
        };
        let required = standard_pset_keys(class);
        if required.is_empty() {
            continue;
        }
        let element_props = props.get(id);
        for key in required {
            let present = element_props
                .and_then(|ep| ep.get(pset_name, key))
                .is_some();
            if !present {
                out.push(ValidationFinding {
                    severity: ValidationSeverity::Warning,
                    code: format!("BIM_MISSING_PSET_{}_{}", pset_name, key),
                    element: Some(id.clone()),
                    description: format!(
                        "{} on {} is missing required property {}.{}",
                        class.ifc_tag(),
                        id.as_str(),
                        pset_name,
                        key
                    ),
                    suggestion: Some(format!("Add {} via the property editor", key)),
                });
            }
        }
    }
    out
}

/// Building elements that are classified but not contained in any
/// spatial structure (no `IfcRelContainedInSpatialStructure`).
fn check_orphan_elements(
    project: &Project,
    classification: &ClassificationStore,
    relations: &RelationStore,
) -> Vec<ValidationFinding> {
    let mut contained: HashSet<EntityId> = HashSet::new();
    for n in project.nodes.values() {
        for e in &n.elements {
            contained.insert(e.clone());
        }
    }
    for r in relations.containments() {
        for e in &r.elements {
            contained.insert(e.clone());
        }
    }
    let mut out = Vec::new();
    for (id, asg) in classification.iter() {
        if !asg.class.is_building_element() {
            continue;
        }
        if !contained.contains(id) {
            out.push(ValidationFinding {
                severity: ValidationSeverity::Warning,
                code: "BIM_ORPHAN_ELEMENT".into(),
                element: Some(id.clone()),
                description: format!(
                    "{} {} is not contained in any spatial structure",
                    asg.class.ifc_tag(),
                    id.as_str()
                ),
                suggestion: Some("Attach the element to a storey or space".into()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification::IfcClass;
    use crate::properties::{PropertySet, PropertyValue};
    use crate::relations::{AggregateRelation, ContainmentRelation};

    fn fixture() -> (Project, ClassificationStore, PropertyStore, RelationStore) {
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let site = p.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
        let bldg = p
            .add_child(&site, IfcClass::IfcBuilding, "Building A")
            .unwrap();
        let storey = p
            .add_child(&bldg, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        p.set_ifc_guid(&root, "01ABC");
        p.set_ifc_guid(&storey, "01STR");
        let element_id = EntityId::new();
        p.attach_element(&storey, element_id.clone());
        let mut cls = ClassificationStore::default();
        cls.assign_manual(element_id.clone(), IfcClass::IfcWall);
        let mut props = PropertyStore::new();
        let mut p_wall = PropertySet::new("Pset_WallCommon");
        p_wall.set("LoadBearing", PropertyValue::Boolean(true));
        p_wall.set("IsExternal", PropertyValue::Boolean(false));
        p_wall.set("Reference", PropertyValue::Label("W-01".into()));
        p_wall.set("ThermalTransmittance", PropertyValue::Real(0.2));
        p_wall.set("FireRating", PropertyValue::Label("EI60".into()));
        p_wall.set("AcousticRating", PropertyValue::Label("Rw45".into()));
        props.entry(element_id.clone()).upsert_pset(p_wall);
        let mut rels = RelationStore::new();
        rels.upsert_containment(ContainmentRelation {
            guid: "rel-1".into(),
            spatial_node: storey.clone(),
            elements: vec![element_id.clone()],
        });
        rels.upsert_aggregate(AggregateRelation {
            guid: "agg-1".into(),
            relating: bldg,
            related: vec![storey],
        });
        (p, cls, props, rels)
    }

    #[test]
    fn clean_project_has_no_errors() {
        let (p, cls, props, rels) = fixture();
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep.is_clean(), "{:?}", rep);
    }

    #[test]
    fn detects_dangling_aggregate_parent() {
        let (p, cls, props, mut rels) = fixture();
        rels.upsert_aggregate(AggregateRelation {
            guid: "ghost".into(),
            relating: EntityId::new(),
            related: vec![EntityId::new()],
        });
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep
            .findings
            .iter()
            .any(|f| f.code == "BIM_DANGLING_AGGREGATE_PARENT"));
    }

    #[test]
    fn detects_missing_classification() {
        let (mut p, cls, props, rels) = fixture();
        let storey = p.nodes_of_class(&IfcClass::IfcBuildingStorey)[0].id.clone();
        let new_el = EntityId::new();
        p.attach_element(&storey, new_el);
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep
            .findings
            .iter()
            .any(|f| f.code == "BIM_MISSING_CLASSIFICATION"));
    }

    #[test]
    fn detects_duplicate_guids() {
        let (mut p, cls, props, rels) = fixture();
        let storey = p.nodes_of_class(&IfcClass::IfcBuildingStorey)[0].id.clone();
        p.set_ifc_guid(&p.root.clone(), "DUP");
        p.set_ifc_guid(&storey, "DUP");
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep.findings.iter().any(|f| f.code == "BIM_DUPLICATE_GUID"));
    }

    #[test]
    fn detects_missing_required_pset_key() {
        // Fresh project with one wall element that has Pset_WallCommon
        // missing the required FireRating key.
        let mut p = Project::new("Demo");
        let root = p.root.clone();
        let storey = p
            .add_child(&root, IfcClass::IfcBuildingStorey, "L01")
            .unwrap();
        let wall_id = EntityId::new();
        p.attach_element(&storey, wall_id.clone());
        let mut cls = ClassificationStore::default();
        cls.assign_manual(wall_id.clone(), IfcClass::IfcWall);
        let mut props = PropertyStore::new();
        let mut p_wall = PropertySet::new("Pset_WallCommon");
        p_wall.set("Reference", PropertyValue::Label("W-01".into()));
        // FireRating, LoadBearing, IsExternal, ThermalTransmittance,
        // AcousticRating are all missing.
        props.entry(wall_id.clone()).upsert_pset(p_wall);
        let mut rels = RelationStore::new();
        rels.upsert_containment(crate::relations::ContainmentRelation {
            guid: "rel".into(),
            spatial_node: storey,
            elements: vec![wall_id],
        });
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep.findings.iter().any(|f| f.code.contains("FireRating")));
        assert!(rep.findings.iter().any(|f| f.code.contains("LoadBearing")));
    }

    #[test]
    fn detects_orphan_element() {
        let (p, mut cls, props, rels) = fixture();
        let stray = EntityId::new();
        cls.assign_manual(stray.clone(), IfcClass::IfcDoor);
        let rep = validate_project(&p, &cls, &props, &rels);
        assert!(rep
            .findings
            .iter()
            .any(|f| f.code == "BIM_ORPHAN_ELEMENT" && f.element.as_ref() == Some(&stray)));
    }
}
