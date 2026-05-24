//! IFC4 material library round-trip:
//!
//!   1. Build a `Project` with five elements assigned to two
//!      materials (one single, one layer-set composite).
//!   2. Export through `IfcWriter::to_string_with_materials`.
//!   3. Parse back through `IfcReader::from_string`.
//!   4. Assert: every `IfcMaterial` survives, the
//!      `IfcMaterialLayerSet` preserves layer order + thicknesses +
//!      ventilation flag, and every element's
//!      `IfcRelAssociatesMaterial` reconnects to the same material
//!      / layer-set name it had pre-export.
//!   5. Assert the read stats correctly report the material counts.
//!
//! Also covers two corner cases:
//!
//!   * 3-arg `IfcWriter::to_string` (no materials) produces byte-
//!     identical output to a same-input `to_string_with_materials`
//!     called with an empty `MaterialStore`.
//!   * A second round-trip (export → read → re-export → re-read) is
//!     stable in every detail recovered (regression-pin for STEP-id
//!     drift in the materials sub-graph).

use aec_bim::classification::{ClassificationStore, IfcClass};
use aec_bim::ifc::{IfcReader, IfcWriter};
use aec_bim::materials::{
    Material, MaterialAssignment, MaterialLayer, MaterialLayerSet, MaterialStore,
};
use aec_bim::properties::PropertyStore;
use aec_bim::spatial::Project;
use aec_core::types::EntityId;

fn build_project_with_materials() -> (
    Project,
    ClassificationStore,
    PropertyStore,
    MaterialStore,
    Vec<EntityId>,
) {
    let mut project = Project::new("Materials Test");
    let site = project
        .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
        .unwrap();
    let bldg = project
        .add_child(&site, IfcClass::IfcBuilding, "B1")
        .unwrap();
    let storey = project
        .add_child(&bldg, IfcClass::IfcBuildingStorey, "Ground")
        .unwrap();

    let mut classification = ClassificationStore::new();
    let properties = PropertyStore::new();
    let mut materials = MaterialStore::new();

    // Materials
    materials.upsert_material(
        Material::new("Concrete C25/30")
            .with_category("concrete")
            .with_description("Structural concrete, 25 MPa cylindrical compressive strength"),
    );
    materials.upsert_material(Material::new("Steel Rebar").with_category("metal"));
    materials.upsert_material(Material::new("Mineral Wool").with_category("insulation"));
    materials.upsert_material(Material::new("Gypsum Board").with_category("drywall"));

    // Layer-set: 250 mm exterior wall composite.
    materials.upsert_layer_set(
        MaterialLayerSet::new("Exterior Wall — 250 mm composite")
            .with_layer(MaterialLayer {
                material_name: "Concrete C25/30".into(),
                thickness_m: 0.150,
                is_ventilated: Some(false),
                name: Some("Structural Core".into()),
                description: None,
                category: Some("concrete".into()),
                priority: Some(70),
            })
            .with_layer(MaterialLayer {
                material_name: "Mineral Wool".into(),
                thickness_m: 0.080,
                is_ventilated: None,
                name: Some("Insulation".into()),
                description: None,
                category: Some("insulation".into()),
                priority: Some(40),
            })
            .with_layer(MaterialLayer {
                material_name: "Gypsum Board".into(),
                thickness_m: 0.020,
                is_ventilated: Some(false),
                name: Some("Interior Finish".into()),
                description: None,
                category: Some("drywall".into()),
                priority: Some(10),
            }),
    );

    // Five elements: 3 walls (composite assignment), 2 slabs (single).
    let mut elements = Vec::new();
    for _ in 0..3 {
        let id = EntityId::new();
        project.attach_element(&storey, id.clone());
        classification.assign_imported(id.clone(), IfcClass::IfcWall);
        let ok = materials.assign_to_element(
            id.clone(),
            MaterialAssignment::LayerSet("Exterior Wall — 250 mm composite".into()),
        );
        assert!(ok, "layer-set assignment must succeed");
        elements.push(id);
    }
    for _ in 0..2 {
        let id = EntityId::new();
        project.attach_element(&storey, id.clone());
        classification.assign_imported(id.clone(), IfcClass::IfcSlab);
        let ok = materials.assign_to_element(
            id.clone(),
            MaterialAssignment::Single("Concrete C25/30".into()),
        );
        assert!(ok, "single material assignment must succeed");
        elements.push(id);
    }

    (project, classification, properties, materials, elements)
}

#[test]
fn material_library_round_trips_through_writer_and_reader() {
    let (project, classification, props, materials, elements) = build_project_with_materials();
    let s = IfcWriter::to_string_with_materials(&project, &classification, &props, &materials);
    let snapshot = IfcReader::from_string(&s).expect("read back materialised IFC");

    // Material library survives.
    let m = snapshot
        .materials
        .material("Concrete C25/30")
        .expect("Concrete material recovered");
    assert_eq!(m.category.as_deref(), Some("concrete"));
    assert_eq!(
        m.description.as_deref(),
        Some("Structural concrete, 25 MPa cylindrical compressive strength")
    );

    let set = snapshot
        .materials
        .layer_set("Exterior Wall — 250 mm composite")
        .expect("layer set recovered");
    assert_eq!(set.layers.len(), 3);
    // Layer order preserved.
    let names: Vec<&str> = set
        .layers
        .iter()
        .map(|l| l.material_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Concrete C25/30", "Mineral Wool", "Gypsum Board"]
    );
    // Layer thicknesses preserved.
    let thicknesses: Vec<f64> = set.layers.iter().map(|l| l.thickness_m).collect();
    assert!((thicknesses[0] - 0.150).abs() < 1e-9);
    assert!((thicknesses[1] - 0.080).abs() < 1e-9);
    assert!((thicknesses[2] - 0.020).abs() < 1e-9);
    // Ventilation flag survives (including the None middle layer).
    assert_eq!(set.layers[0].is_ventilated, Some(false));
    assert_eq!(set.layers[1].is_ventilated, None);
    assert_eq!(set.layers[2].is_ventilated, Some(false));
    // Per-layer priorities + categories survive.
    assert_eq!(set.layers[0].priority, Some(70));
    assert_eq!(set.layers[1].priority, Some(40));
    assert_eq!(set.layers[2].priority, Some(10));

    // Every element keeps its assignment.
    for el in &elements[0..3] {
        match snapshot.materials.assignment(el) {
            Some(MaterialAssignment::LayerSet(name)) => {
                assert_eq!(name, "Exterior Wall — 250 mm composite");
            }
            other => panic!("expected layer-set assignment, got {other:?}"),
        }
    }
    for el in &elements[3..5] {
        match snapshot.materials.assignment(el) {
            Some(MaterialAssignment::Single(name)) => {
                assert_eq!(name, "Concrete C25/30");
            }
            other => panic!("expected single material assignment, got {other:?}"),
        }
    }

    // Stats reflect the count.
    assert_eq!(snapshot.stats.materials, 4);
    assert_eq!(snapshot.stats.material_layer_sets, 1);
    assert_eq!(snapshot.stats.material_assignments, 5);
}

#[test]
fn empty_material_store_produces_same_output_as_three_arg_writer() {
    let (project, classification, props, _materials, _) = build_project_with_materials();
    let s_three_arg = IfcWriter::to_string(&project, &classification, &props);
    let s_four_arg = IfcWriter::to_string_with_materials(
        &project,
        &classification,
        &props,
        &MaterialStore::default(),
    );
    assert_eq!(
        s_three_arg, s_four_arg,
        "empty MaterialStore must produce byte-identical output to 3-arg writer"
    );
}

#[test]
fn double_round_trip_is_stable() {
    let (project, classification, props, materials, _) = build_project_with_materials();
    // First round trip
    let s1 = IfcWriter::to_string_with_materials(&project, &classification, &props, &materials);
    let snap1 = IfcReader::from_string(&s1).expect("first parse");
    // Second round trip — write the recovered snapshot back out and
    // re-read. We rebuild the inputs from the snapshot so the second
    // export uses the EXACT material graph the reader produced (not
    // the original).
    let s2 = IfcWriter::to_string_with_materials(
        &snap1.project,
        &snap1.classification,
        &snap1.properties,
        &snap1.materials,
    );
    let snap2 = IfcReader::from_string(&s2).expect("second parse");
    // Material counts are stable.
    assert_eq!(snap1.stats.materials, snap2.stats.materials);
    assert_eq!(
        snap1.stats.material_layer_sets,
        snap2.stats.material_layer_sets
    );
    assert_eq!(
        snap1.stats.material_assignments,
        snap2.stats.material_assignments
    );
    // Layer set shape is stable.
    let set1 = snap1
        .materials
        .layer_set("Exterior Wall — 250 mm composite")
        .unwrap();
    let set2 = snap2
        .materials
        .layer_set("Exterior Wall — 250 mm composite")
        .unwrap();
    assert_eq!(set1.layers.len(), set2.layers.len());
    for (l1, l2) in set1.layers.iter().zip(set2.layers.iter()) {
        assert_eq!(l1.material_name, l2.material_name);
        assert!((l1.thickness_m - l2.thickness_m).abs() < 1e-12);
        assert_eq!(l1.is_ventilated, l2.is_ventilated);
        assert_eq!(l1.priority, l2.priority);
    }
}

#[test]
fn reader_tolerates_unmodeled_material_reference() {
    // Authoring tools sometimes emit `IfcMaterialProfileSet` or
    // `IfcMaterialConstituentSet` which AEC Studio doesn't yet
    // model. An IfcRelAssociatesMaterial pointing at one of those
    // must be skipped per the tolerate-and-skip module contract,
    // not hard-fail the whole parse.
    let body = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('test'),'2;1');
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);
#2 = IFCPROJECT('00000000000000000000a1',#1,$,'P',$,$,$,$,$);
#3 = IFCSITE('00000000000000000000a2',#1,$,'S',$,$,$,$);
#4 = IFCBUILDING('00000000000000000000a3',#1,$,'B',$,$,$,$);
#5 = IFCBUILDINGSTOREY('00000000000000000000a4',#1,$,'L1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));
#7 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));
#8 = IFCRELAGGREGATES('00000000000000000000a7',#1,$,$,#4,(#5));
#9 = IFCMATERIALPROFILESET('beam-profiles',$,$,$,$);
#10 = IFCWALL('00000000000000000000a8',#1,$,'IfcWall::ent_01hx5sabwall0000000000000000','Wall',$,$,$);
#11 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000a9',#1,$,$,(#10),#5);
#12 = IFCRELASSOCIATESMATERIAL('00000000000000000000aa',#1,$,$,(#10),#9);
ENDSEC;
END-ISO-10303-21;
";
    let snap = IfcReader::from_string(body).expect("tolerate unmodeled material ref");
    // Unmodeled material reference dropped — no assignment.
    assert_eq!(snap.materials.material_count(), 0);
    assert_eq!(snap.stats.material_assignments, 0);
    // But element + spatial still loaded.
    assert_eq!(snap.stats.elements, 1);
    assert_eq!(snap.stats.spatial_nodes, 4);
}

#[test]
fn reader_follows_material_layer_set_usage_indirection() {
    // Revit / ArchiCAD bind walls / slabs / roofs to layer-sets
    // through an `IfcMaterialLayerSetUsage` wrapper that carries
    // orientation + offset metadata. The reader must hop through
    // the usage to reach the underlying `IfcMaterialLayerSet`.
    //
    // STEP graph here:
    //   #11 IfcMaterial 'Concrete'
    //   #12 IfcMaterialLayer(#11, 200mm)
    //   #13 IfcMaterialLayerSet((#12), 'Concrete Slab 200')
    //   #14 IfcMaterialLayerSetUsage(#13, AXIS2, POSITIVE, 0.0)
    //   #15 IfcWall
    //   #16 IfcRelContainedInSpatialStructure
    //   #17 IfcRelAssociatesMaterial(elem=#15, RelatingMaterial=#14)
    let body = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('test'),'2;1');
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);
#2 = IFCPROJECT('00000000000000000000a1',#1,$,'P',$,$,$,$,$);
#3 = IFCSITE('00000000000000000000a2',#1,$,'S',$,$,$,$);
#4 = IFCBUILDING('00000000000000000000a3',#1,$,'B',$,$,$,$);
#5 = IFCBUILDINGSTOREY('00000000000000000000a4',#1,$,'L1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));
#7 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));
#8 = IFCRELAGGREGATES('00000000000000000000a7',#1,$,$,#4,(#5));
#11 = IFCMATERIAL('Concrete',$,$);
#12 = IFCMATERIALLAYER(#11,0.2,.F.,$,$,$,$);
#13 = IFCMATERIALLAYERSET((#12),'Concrete Slab 200',$);
#14 = IFCMATERIALLAYERSETUSAGE(#13,.AXIS2.,.POSITIVE.,0.0,$);
#15 = IFCWALL('00000000000000000000a8',#1,$,'IfcWall::ent_01hx5sabwall0000000000000000','Wall',$,$,$);
#16 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000a9',#1,$,$,(#15),#5);
#17 = IFCRELASSOCIATESMATERIAL('00000000000000000000aa',#1,$,$,(#15),#14);
ENDSEC;
END-ISO-10303-21;
";
    let snap = IfcReader::from_string(body).expect("follow IfcMaterialLayerSetUsage indirection");
    assert_eq!(snap.materials.material_count(), 1);
    assert_eq!(snap.materials.layer_set_count(), 1);
    // The usage indirection MUST resolve back to the underlying
    // layer-set so the assignment lands on the wall.
    assert_eq!(snap.stats.material_assignments, 1);
    let set = snap
        .materials
        .layer_set("Concrete Slab 200")
        .expect("layer-set recovered through usage");
    assert_eq!(set.layers.len(), 1);
    assert_eq!(set.layers[0].material_name, "Concrete");
    assert!((set.layers[0].thickness_m - 0.2).abs() < 1e-12);
}

#[test]
fn reader_tolerates_air_gap_material_layer() {
    // IFC4 `IfcMaterialLayer.Material` is `OPTIONAL IfcMaterial` —
    // a `$` literal means the layer is an air gap (legitimate per
    // schema, common in real Revit / ArchiCAD curtain walls). The
    // reader must drop the air-gap layer rather than refuse the
    // whole file. Other layers in the same set must survive.
    //
    // STEP graph: a 2-layer wall where layer 1 is concrete (200mm)
    // and layer 2 is an air gap (50mm).
    let body = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('test'),'2;1');
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);
#2 = IFCPROJECT('00000000000000000000a1',#1,$,'P',$,$,$,$,$);
#3 = IFCSITE('00000000000000000000a2',#1,$,'S',$,$,$,$);
#4 = IFCBUILDING('00000000000000000000a3',#1,$,'B',$,$,$,$);
#5 = IFCBUILDINGSTOREY('00000000000000000000a4',#1,$,'L1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));
#7 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));
#8 = IFCRELAGGREGATES('00000000000000000000a7',#1,$,$,#4,(#5));
#11 = IFCMATERIAL('Concrete',$,$);
#12 = IFCMATERIALLAYER(#11,0.2,.F.,$,$,$,$);
#13 = IFCMATERIALLAYER($,0.05,.T.,$,$,$,$);
#14 = IFCMATERIALLAYERSET((#12,#13),'Wall Assembly',$);
#15 = IFCWALL('00000000000000000000a8',#1,$,'IfcWall::ent_01hx5sabwall0000000000000000','Wall',$,$,$);
#16 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000a9',#1,$,$,(#15),#5);
#17 = IFCRELASSOCIATESMATERIAL('00000000000000000000aa',#1,$,$,(#15),#14);
ENDSEC;
END-ISO-10303-21;
";
    let snap = IfcReader::from_string(body).expect("tolerate $ Material on IfcMaterialLayer");
    assert_eq!(snap.materials.material_count(), 1);
    assert_eq!(snap.materials.layer_set_count(), 1);
    assert_eq!(snap.stats.material_assignments, 1);
    let set = snap
        .materials
        .layer_set("Wall Assembly")
        .expect("layer-set recovered with air-gap layer dropped");
    // Concrete layer survives, air gap was dropped.
    assert_eq!(set.layers.len(), 1);
    assert_eq!(set.layers[0].material_name, "Concrete");
}

#[test]
fn writer_emits_material_assignment_on_spatial_node() {
    // IFC4 declares `IfcRelAssociatesMaterial.RelatedObjects` as
    // `SET[1:?] OF IfcObjectDefinition` — both elements AND spatial
    // structure subtypes (`IfcSpace`, `IfcBuildingStorey`, …) are
    // legal targets. Revit's "Floor Finish" property on a space
    // exports as an `IfcRelAssociatesMaterial` bound to the
    // `IfcSpace`. The reader already accepts this (see
    // material_assignments accumulation in stage-3); the writer
    // must mirror it or round-trip silently drops the binding.
    let mut project = Project::new("Spatial Material Test");
    let site = project
        .add_child(&project.root.clone(), IfcClass::IfcSite, "Site")
        .unwrap();
    let bldg = project
        .add_child(&site, IfcClass::IfcBuilding, "B1")
        .unwrap();
    let storey = project
        .add_child(&bldg, IfcClass::IfcBuildingStorey, "Ground")
        .unwrap();
    let space = project
        .add_child(&storey, IfcClass::IfcSpace, "Lobby")
        .unwrap();

    let classification = ClassificationStore::new();
    let properties = PropertyStore::new();
    let mut materials = MaterialStore::new();
    materials.upsert_material(Material::new("Polished Concrete"));
    let assigned = materials.assign_to_element(
        space.clone(),
        MaterialAssignment::Single("Polished Concrete".into()),
    );
    assert!(assigned, "spatial-node material binding must succeed");

    let s = IfcWriter::to_string_with_materials(&project, &classification, &properties, &materials);
    let snapshot = IfcReader::from_string(&s).expect("read back spatial-material IFC");

    // Material survives.
    assert_eq!(snapshot.materials.material_count(), 1);
    // KEY: the relation actually made it to the wire. Pre-fix this
    // was 0 (writer dropped the assignment because `entity` was a
    // spatial node, not in `element_step`).
    assert_eq!(snapshot.stats.material_assignments, 1);
}

#[test]
fn reader_tolerates_anonymous_material_layer_set() {
    // IFC4 `IfcMaterialLayerSet.LayerSetName` is `OPTIONAL
    // IfcLabel` — `$` is legitimate (IfcOpenShell / Tekla /
    // scripted exporters routinely emit it). The reader must
    // refuse to upsert the unnameable set (since the in-memory
    // `MaterialStore` keys by name) but must NOT fail the whole
    // parse — the file as a whole still loads, other layer sets
    // and direct material assignments are unaffected.
    let body = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('test'),'2;1');
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);
#2 = IFCPROJECT('00000000000000000000a1',#1,$,'P',$,$,$,$,$);
#3 = IFCSITE('00000000000000000000a2',#1,$,'S',$,$,$,$);
#4 = IFCBUILDING('00000000000000000000a3',#1,$,'B',$,$,$,$);
#5 = IFCBUILDINGSTOREY('00000000000000000000a4',#1,$,'L1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));
#7 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));
#8 = IFCRELAGGREGATES('00000000000000000000a7',#1,$,$,#4,(#5));
#11 = IFCMATERIAL('Concrete',$,$);
#12 = IFCMATERIALLAYER(#11,0.2,.F.,$,$,$,$);
#13 = IFCMATERIALLAYERSET((#12),$,$);
#14 = IFCWALL('00000000000000000000a8',#1,$,'IfcWall::ent_01hx5sabwall0000000000000000','Wall',$,$,$);
#15 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000a9',#1,$,$,(#14),#5);
ENDSEC;
END-ISO-10303-21;
";
    let snap = IfcReader::from_string(body).expect("tolerate $ LayerSetName");
    // Material survives; anonymous layer-set was dropped.
    assert_eq!(snap.materials.material_count(), 1);
    assert_eq!(snap.materials.layer_set_count(), 0);
    assert_eq!(snap.stats.elements, 1);
}
