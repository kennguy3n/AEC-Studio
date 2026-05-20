//! Task 26 — IFC roundtrip GUID preservation.
//!
//! Builds a real `Project` with 50 building elements (walls, slabs,
//! doors, windows, furniture) across two storeys, exports the file
//! through the in-process IFC writer, parses it back through the
//! reader, and asserts:
//!
//!   1. Every spatial node's GUID is preserved verbatim across the
//!      round-trip.
//!   2. Every element's GUID is preserved verbatim, and matches the
//!      GUID derived from its `EntityId`.
//!   3. The spatial hierarchy reconnects exactly (Project → Site →
//!      Building → Storey → Space) and each storey's element list is
//!      preserved with the original count.
//!   4. Every Pset and Qto round-trips with the same name, key set,
//!      and `PropertyValue` typing.
//!
//! Then we modify five elements (additional Pset entries plus a new
//! Qto value), re-export, and assert:
//!
//!   * Unmodified elements keep the **same** GUID *and* identical
//!     property payloads.
//!   * Modified elements keep their original GUID (per IFC4 spec) and
//!     surface the new properties in the next parse.

use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::ifc::{compress_entity_id_to_guid, IfcReader, IfcWriter};
use aec_bim::properties::{PropertySet, PropertyStore, PropertyValue, QuantitySet};
use aec_bim::spatial::Project;
use aec_core::types::EntityId;

#[derive(Clone)]
struct ElementSpec {
    class: IfcClass,
    storey: usize,
    pset_name: &'static str,
    pset: Vec<(&'static str, PropertyValue)>,
    qset_name: &'static str,
    qset: Vec<(&'static str, PropertyValue)>,
}

fn wall(i: usize, storey: usize) -> ElementSpec {
    ElementSpec {
        class: IfcClass::IfcWall,
        storey,
        pset_name: "Pset_WallCommon",
        pset: vec![
            ("Reference", PropertyValue::Label(format!("W-{:03}", i))),
            ("LoadBearing", PropertyValue::Boolean(i % 2 == 0)),
            ("Material", PropertyValue::Text("concrete".into())),
        ],
        qset_name: "Qto_WallBaseQuantities",
        qset: vec![
            ("Length", PropertyValue::Length(3.0 + (i as f64) * 0.05)),
            ("NetSideArea", PropertyValue::Area(8.0 + (i as f64) * 0.1)),
            ("NetVolume", PropertyValue::Volume(0.8 + (i as f64) * 0.01)),
        ],
    }
}

fn slab(i: usize, storey: usize) -> ElementSpec {
    ElementSpec {
        class: IfcClass::IfcSlab,
        storey,
        pset_name: "Pset_SlabCommon",
        pset: vec![
            ("Reference", PropertyValue::Label(format!("SL-{:03}", i))),
            ("IsExternal", PropertyValue::Boolean(false)),
        ],
        qset_name: "Qto_SlabBaseQuantities",
        qset: vec![
            ("NetArea", PropertyValue::Area(45.0)),
            ("NetVolume", PropertyValue::Volume(9.0)),
        ],
    }
}

fn door(i: usize, storey: usize) -> ElementSpec {
    ElementSpec {
        class: IfcClass::IfcDoor,
        storey,
        pset_name: "Pset_DoorCommon",
        pset: vec![
            ("Reference", PropertyValue::Label(format!("D-{:03}", i))),
            ("FireRating", PropertyValue::Label("FD30".into())),
            ("OperationType", PropertyValue::Label("SINGLE_SWING".into())),
        ],
        qset_name: "Qto_DoorBaseQuantities",
        qset: vec![
            ("Width", PropertyValue::Length(0.9)),
            ("Height", PropertyValue::Length(2.1)),
            ("Area", PropertyValue::Area(1.89)),
        ],
    }
}

fn window(i: usize, storey: usize) -> ElementSpec {
    ElementSpec {
        class: IfcClass::IfcWindow,
        storey,
        pset_name: "Pset_WindowCommon",
        pset: vec![
            ("Reference", PropertyValue::Label(format!("WN-{:03}", i))),
            ("IsExternal", PropertyValue::Boolean(true)),
        ],
        qset_name: "Qto_WindowBaseQuantities",
        qset: vec![
            ("Width", PropertyValue::Length(1.2)),
            ("Height", PropertyValue::Length(1.5)),
            ("Area", PropertyValue::Area(1.8)),
        ],
    }
}

fn furniture(i: usize, storey: usize) -> ElementSpec {
    ElementSpec {
        class: IfcClass::IfcFurnishingElement,
        storey,
        pset_name: "Pset_FurnitureCommon",
        pset: vec![
            ("Reference", PropertyValue::Label(format!("F-{:03}", i))),
            ("Manufacturer", PropertyValue::Text("Aspen Furniture".into())),
        ],
        qset_name: "Qto_FurnitureBaseQuantities",
        qset: vec![("NetVolume", PropertyValue::Volume(0.25))],
    }
}

fn build_50_element_project() -> (
    Project,
    ClassificationStore,
    PropertyStore,
    Vec<EntityId>, // element_ids in insertion order
    Vec<EntityId>, // [site, building, storey_a, storey_b]
) {
    let mut project = Project::new("IFC Roundtrip Subject");
    let root = project.root.clone();
    let site = project
        .add_child(&root, IfcClass::IfcSite, "Site")
        .expect("site");
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Building A")
        .expect("building");
    let storey_a = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "L01")
        .expect("storey a");
    let storey_b = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "L02")
        .expect("storey b");
    let spaces_a: Vec<EntityId> = (0..3)
        .map(|i| {
            project
                .add_child(
                    &storey_a,
                    IfcClass::IfcSpace,
                    format!("Room L01-{:02}", i + 1),
                )
                .expect("space a")
        })
        .collect();
    let spaces_b: Vec<EntityId> = (0..2)
        .map(|i| {
            project
                .add_child(
                    &storey_b,
                    IfcClass::IfcSpace,
                    format!("Room L02-{:02}", i + 1),
                )
                .expect("space b")
        })
        .collect();
    let _ = (spaces_a, spaces_b);
    let storeys = [storey_a.clone(), storey_b.clone()];
    let mut classification = ClassificationStore::new();
    let mut props = PropertyStore::new();

    // ---- 50 elements ----
    // 20 walls (10/floor), 10 slabs (5/floor), 8 doors (4/floor),
    // 8 windows (4/floor), 4 furniture (2/floor). Total = 50.
    let mut specs: Vec<ElementSpec> = Vec::with_capacity(50);
    for i in 0..20 {
        specs.push(wall(i, i % 2));
    }
    for i in 0..10 {
        specs.push(slab(i, i % 2));
    }
    for i in 0..8 {
        specs.push(door(i, i % 2));
    }
    for i in 0..8 {
        specs.push(window(i, i % 2));
    }
    for i in 0..4 {
        specs.push(furniture(i, i % 2));
    }
    assert_eq!(specs.len(), 50);

    let mut element_ids: Vec<EntityId> = Vec::with_capacity(50);
    for spec in &specs {
        let id = EntityId::new();
        element_ids.push(id.clone());
        project.attach_element(&storeys[spec.storey], id.clone());
        classification.assign_manual(id.clone(), spec.class.clone());
        let mut ps = PropertySet::new(spec.pset_name);
        for (k, v) in &spec.pset {
            ps.set(*k, v.clone());
        }
        props.entry(id.clone()).upsert_pset(ps);
        let mut qs = QuantitySet::new(spec.qset_name);
        for (k, v) in &spec.qset {
            qs.quantities.insert((*k).into(), v.clone());
        }
        props.entry(id.clone()).upsert_qset(qs);
    }

    let spatial = vec![site, building, storey_a, storey_b];
    (project, classification, props, element_ids, spatial)
}

#[test]
fn ifc_roundtrip_preserves_guids_psets_and_spatial_graph() {
    let (project, classification, props, element_ids, spatial_nodes) =
        build_50_element_project();
    let s = IfcWriter::to_string(&project, &classification, &props);

    // Sanity: byte-level envelope.
    assert!(s.starts_with("ISO-10303-21;"), "STEP envelope");
    assert!(
        s.trim_end().ends_with("END-ISO-10303-21;"),
        "STEP terminator"
    );
    assert!(s.contains("FILE_SCHEMA(('IFC4'))"), "IFC4 schema");

    let snap = IfcReader::from_string(&s).expect("parses our own output");
    let stats = &snap.stats;

    assert_eq!(stats.elements, 50, "all 50 elements parsed");
    // Spatial: Project(1) + Site(1) + Building(1) + Storey(2) + Space(5) = 10
    assert_eq!(stats.spatial_nodes, 10, "10 spatial nodes");
    assert_eq!(stats.psets, 50, "one Pset per element");
    assert_eq!(stats.qsets, 50, "one Qto per element");
    // Aggregations: root→site, site→bldg, bldg→storeyA, bldg→storeyB,
    // storeyA→3 spaces (as one IFCRELAGGREGATES list), storeyB→2 spaces.
    assert_eq!(stats.aggregations, 9, "expected aggregation edges");
    assert_eq!(stats.containments, 50, "every element contained in a storey");

    // (1) Every element GUID matches what compress() would derive
    // from the original EntityId.
    for el in &element_ids {
        let g = snap
            .guid_by_entity
            .get(el)
            .expect("parsed element GUID by original EntityId");
        assert_eq!(
            g,
            &compress_entity_id_to_guid(el),
            "element {} GUID stable across roundtrip",
            el
        );
    }

    // (2) Every spatial node's GUID is preserved. We can't key by the
    // original EntityId (spatial nodes lose EntityId encoding on
    // export), but we can index by class+name.
    let written_guids: std::collections::HashMap<(IfcClass, String), String> = project
        .nodes
        .values()
        .map(|n| {
            (
                (n.class.clone(), n.name.clone()),
                n.ifc_guid
                    .clone()
                    .unwrap_or_else(|| compress_entity_id_to_guid(&n.id)),
            )
        })
        .collect();
    for n in snap.project.nodes.values() {
        let want = written_guids
            .get(&(n.class.clone(), n.name.clone()))
            .expect("spatial node identified");
        let got = n
            .ifc_guid
            .as_deref()
            .expect("parsed spatial nodes carry a GUID");
        assert_eq!(got, want, "spatial node {} GUID round-trip", n.name);
    }

    // (3) Pset + Qto + classification round-trip.
    for el in &element_ids {
        let original = props.get(el).expect("source props");
        let parsed = snap.properties.get(el).expect("round-tripped props");
        assert_eq!(
            original.psets.keys().collect::<Vec<_>>(),
            parsed.psets.keys().collect::<Vec<_>>(),
            "Pset names match for {}",
            el
        );
        for (pname, ps) in &original.psets {
            let pp = parsed.psets.get(pname).expect("Pset present");
            assert_eq!(
                ps.properties.keys().collect::<Vec<_>>(),
                pp.properties.keys().collect::<Vec<_>>(),
                "Pset {} keys match",
                pname
            );
            for (k, v) in &ps.properties {
                let vv = pp.properties.get(k).expect("key present");
                assert_eq!(v, vv, "Pset value {}::{}", pname, k);
            }
        }
        for (qname, qs) in &original.qsets {
            let qq = parsed.qsets.get(qname).expect("Qto present");
            for (k, v) in &qs.quantities {
                let vv = qq.quantities.get(k).expect("Qto key present");
                assert_eq!(v, vv, "Qto value {}::{}", qname, k);
            }
        }
    }

    // Classification mirrors via assign_imported, but the IfcClass
    // itself must match.
    let _ = spatial_nodes;
    for el in &element_ids {
        let want = classification.accepted_for(el).expect("orig class");
        let got = snap
            .classification
            .accepted_for(el)
            .expect("round-tripped class");
        assert_eq!(want, got, "element class round-trip {}", el);
    }
}

#[test]
fn ifc_modifying_five_elements_preserves_unmodified_guids_and_surfaces_new_props() {
    let (mut project, classification, mut props, element_ids, _spatial) =
        build_50_element_project();

    // Capture the original GUIDs so we can compare after modification.
    let pass1 = IfcWriter::to_string(&project, &classification, &props);
    let snap1 = IfcReader::from_string(&pass1).expect("parses pass 1");
    let original_guids: std::collections::HashMap<EntityId, String> = element_ids
        .iter()
        .map(|e| (e.clone(), snap1.guid_by_entity[e].clone()))
        .collect();
    // Snapshot the original properties so we can compare unchanged
    // entries verbatim after re-export.
    let original_props = props.clone();

    // Modify five distinct elements:
    //   * Element 0: append a new Pset key (CoatingProtection).
    //   * Element 7: append a brand-new Pset (Pset_QuantityOverride).
    //   * Element 14: change an existing Qto value.
    //   * Element 21: append a brand-new Qto (Pset_FinishesArea).
    //   * Element 35: append a new key to an existing Pset and Qto.
    let modified_indices: [usize; 5] = [0, 7, 14, 21, 35];
    {
        let id = &element_ids[0];
        let mut p = PropertySet::new("Pset_WallCommon");
        for (k, v) in &props.get(id).unwrap().psets["Pset_WallCommon"].properties {
            p.set(k.clone(), v.clone());
        }
        p.set("CoatingProtection", PropertyValue::Label("none".into()));
        props.entry(id.clone()).upsert_pset(p);
    }
    {
        let id = &element_ids[7];
        let mut p = PropertySet::new("Pset_QuantityOverride");
        p.set("Reason", PropertyValue::Text("revision-2".into()));
        props.entry(id.clone()).upsert_pset(p);
    }
    {
        let id = &element_ids[14];
        let mut q = QuantitySet::new("Qto_WallBaseQuantities");
        for (k, v) in
            &props.get(id).unwrap().qsets["Qto_WallBaseQuantities"].quantities
        {
            q.quantities.insert(k.clone(), v.clone());
        }
        q.quantities
            .insert("Length".into(), PropertyValue::Length(99.9));
        props.entry(id.clone()).upsert_qset(q);
    }
    {
        let id = &element_ids[21];
        let mut q = QuantitySet::new("Pset_FinishesArea");
        q.quantities
            .insert("PaintedArea".into(), PropertyValue::Area(12.5));
        props.entry(id.clone()).upsert_qset(q);
    }
    {
        let id = &element_ids[35];
        let mut p = PropertySet::new("Pset_DoorCommon");
        for (k, v) in &props.get(id).unwrap().psets["Pset_DoorCommon"].properties {
            p.set(k.clone(), v.clone());
        }
        p.set("SmokeStop", PropertyValue::Boolean(true));
        props.entry(id.clone()).upsert_pset(p);
        let mut q = QuantitySet::new("Qto_DoorBaseQuantities");
        for (k, v) in
            &props.get(id).unwrap().qsets["Qto_DoorBaseQuantities"].quantities
        {
            q.quantities.insert(k.clone(), v.clone());
        }
        q.quantities
            .insert("Weight".into(), PropertyValue::Real(48.0));
        props.entry(id.clone()).upsert_qset(q);
    }
    let _ = project.set_ifc_guid(&project.root.clone(), "0000000000000000000000");

    let pass2 = IfcWriter::to_string(&project, &classification, &props);
    let snap2 = IfcReader::from_string(&pass2).expect("parses pass 2");
    assert_eq!(
        snap2.stats.elements, 50,
        "modifications must not change element count"
    );

    // Every original element GUID is preserved across the modify
    // → re-export cycle.
    for el in &element_ids {
        let prev = original_guids.get(el).unwrap();
        let now = snap2
            .guid_by_entity
            .get(el)
            .expect("element survives re-export");
        assert_eq!(
            prev, now,
            "element {} keeps its GUID across modification re-export",
            el
        );
    }

    // For the unmodified subset, all property keys and values must
    // be byte-equal between the original and the re-parsed snapshot.
    for (i, el) in element_ids.iter().enumerate() {
        if modified_indices.contains(&i) {
            continue;
        }
        let orig = original_props.get(el).unwrap();
        let parsed = snap2.properties.get(el).unwrap();
        for (pname, ps) in &orig.psets {
            let pp = parsed.psets.get(pname).expect("Pset preserved");
            assert_eq!(
                ps.properties, pp.properties,
                "unmodified element {} Pset {} bytes",
                el, pname
            );
        }
        for (qname, qs) in &orig.qsets {
            let qq = parsed.qsets.get(qname).expect("Qto preserved");
            assert_eq!(
                qs.quantities, qq.quantities,
                "unmodified element {} Qto {} bytes",
                el, qname
            );
        }
    }

    // The modified subset surfaces the new payload while keeping the
    // existing data intact.
    {
        let id = &element_ids[0];
        let p = &snap2.properties.get(id).unwrap().psets["Pset_WallCommon"];
        assert_eq!(
            p.properties.get("CoatingProtection"),
            Some(&PropertyValue::Label("none".into())),
            "elem 0 surfaces new pset key"
        );
        assert!(
            p.properties.contains_key("LoadBearing"),
            "existing pset keys retained on elem 0"
        );
    }
    {
        let id = &element_ids[7];
        let parsed = snap2.properties.get(id).unwrap();
        assert!(
            parsed.psets.contains_key("Pset_QuantityOverride"),
            "elem 7 surfaces brand-new pset"
        );
        assert!(
            parsed.psets.contains_key("Pset_WallCommon"),
            "elem 7 keeps original pset"
        );
    }
    {
        let id = &element_ids[14];
        let q = &snap2.properties.get(id).unwrap().qsets["Qto_WallBaseQuantities"];
        assert_eq!(
            q.quantities.get("Length"),
            Some(&PropertyValue::Length(99.9)),
            "elem 14 surfaces revised Length"
        );
    }
    {
        let id = &element_ids[21];
        let parsed = snap2.properties.get(id).unwrap();
        assert!(
            parsed.qsets.contains_key("Pset_FinishesArea"),
            "elem 21 surfaces brand-new qto"
        );
    }
    {
        let id = &element_ids[35];
        let parsed = snap2.properties.get(id).unwrap();
        assert_eq!(
            parsed.psets["Pset_DoorCommon"].properties.get("SmokeStop"),
            Some(&PropertyValue::Boolean(true)),
            "elem 35 surfaces new pset key"
        );
        assert_eq!(
            parsed.qsets["Qto_DoorBaseQuantities"]
                .quantities
                .get("Weight"),
            Some(&PropertyValue::Real(48.0)),
            "elem 35 surfaces new qto key"
        );
    }
}
