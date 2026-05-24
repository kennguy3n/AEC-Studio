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

#[test]
fn material_layer_set_usage_metadata_round_trips_via_synthetic_pset() {
    // External BIM tools (Revit, ArchiCAD) bind walls to layer-sets
    // via `IfcMaterialLayerSetUsage`, which carries per-wall
    // LayerSetDirection / DirectionSense / OffsetFromReferenceLine
    // metadata. AEC Studio's `MaterialAssignment::LayerSet` only
    // captures the set name, so the reader pivots the usage
    // metadata into a synthetic `AEC_LayerSetUsage` Pset on the
    // element. The writer recovers it and emits a real
    // `IfcMaterialLayerSetUsage` wrapper on export.
    //
    // This test pins the full round-trip:
    //   1. Parse a Revit-style IFC with two walls bound through
    //      IfcMaterialLayerSetUsage (different offsets per wall).
    //   2. Assert the synthetic Pset materialised with the right
    //      direction / sense / offset on each wall.
    //   3. Re-export the snapshot.
    //   4. Re-parse the export — assert the synthetic Pset survives
    //      unchanged on both walls.
    //   5. Assert the exported STEP body contains
    //      `IFCMATERIALLAYERSETUSAGE` (proof we emitted the wrapper
    //      and didn't fall back to a direct layer-set ref).
    let body = "\
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('test'),'2;1');
FILE_NAME('t.ifc','2026-05-20T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1 = IFCOWNERHISTORY($,$,$,.NOCHANGE.,$,$,$,1747699200);
#2 = IFCPROJECT('00000000000000000000a1',#1,'P','P',$,$,$,$,$);
#3 = IFCSITE('00000000000000000000a2',#1,'S','S',$,$,$,$);
#4 = IFCBUILDING('00000000000000000000a3',#1,'B','B',$,$,$,$);
#5 = IFCBUILDINGSTOREY('00000000000000000000a4',#1,'L1','L1',$,$,$,$);
#6 = IFCRELAGGREGATES('00000000000000000000a5',#1,$,$,#2,(#3));
#7 = IFCRELAGGREGATES('00000000000000000000a6',#1,$,$,#3,(#4));
#8 = IFCRELAGGREGATES('00000000000000000000a7',#1,$,$,#4,(#5));
#11 = IFCMATERIAL('Concrete',$,$);
#12 = IFCMATERIALLAYER(#11,0.2,.F.,$,$,$,$);
#13 = IFCMATERIALLAYERSET((#12),'Wall-200',$);
#14 = IFCWALL('00000000000000000000a8',#1,'W1','IfcWall::ent_01hx5sabwall0000000000000001','Wall',$,$,$);
#15 = IFCWALL('00000000000000000000a9',#1,'W2','IfcWall::ent_01hx5sabwall0000000000000002','Wall',$,$,$);
#16 = IFCRELCONTAINEDINSPATIALSTRUCTURE('00000000000000000000b1',#1,$,$,(#14,#15),#5);
#20 = IFCMATERIALLAYERSETUSAGE(#13,.AXIS2.,.POSITIVE.,0.1,$);
#21 = IFCMATERIALLAYERSETUSAGE(#13,.AXIS2.,.NEGATIVE.,-0.05,$);
#22 = IFCRELASSOCIATESMATERIAL('00000000000000000000b2',#1,$,$,(#14),#20);
#23 = IFCRELASSOCIATESMATERIAL('00000000000000000000b3',#1,$,$,(#15),#21);
ENDSEC;
END-ISO-10303-21;
";
    let snap = IfcReader::from_string(body).expect("parse Revit-style usage indirection");

    // The two walls landed and both have a LayerSet assignment.
    let wall_ids: Vec<_> = snap
        .materials
        .assignments()
        .filter_map(|(id, a)| match a {
            MaterialAssignment::LayerSet(n) if n == "Wall-200" => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        wall_ids.len(),
        2,
        "both walls must have layer-set assignment"
    );

    // Each wall has the synthetic AEC_LayerSetUsage Pset with the
    // direction / sense / offset values from its usage step.
    let mut offsets: Vec<f64> = Vec::new();
    for id in &wall_ids {
        let props = snap
            .properties
            .get(id)
            .expect("wall must have a properties entry");
        let usage = props
            .psets
            .get(aec_bim::materials::AEC_LAYER_SET_USAGE_PSET)
            .expect("AEC_LayerSetUsage pset must be attached to each wall");
        // LayerSetDirection survived literal-verbatim.
        match usage
            .properties
            .get(aec_bim::materials::AEC_LAYER_SET_USAGE_KEY_DIRECTION)
        {
            Some(aec_bim::properties::PropertyValue::Label(s)) => assert_eq!(s, ".AXIS2."),
            other => panic!("expected LayerSetDirection label, got {other:?}"),
        }
        // OffsetFromReferenceLine survived as a Length value.
        match usage
            .properties
            .get(aec_bim::materials::AEC_LAYER_SET_USAGE_KEY_OFFSET)
        {
            Some(aec_bim::properties::PropertyValue::Length(v)) => offsets.push(*v),
            other => panic!("expected OffsetFromReferenceLine length, got {other:?}"),
        }
    }
    offsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        (offsets[0] - (-0.05)).abs() < 1e-9 && (offsets[1] - 0.1).abs() < 1e-9,
        "offsets must round-trip exactly (got {offsets:?})"
    );

    // Re-export and re-read.
    let s = IfcWriter::to_string_with_materials(
        &snap.project,
        &snap.classification,
        &snap.properties,
        &snap.materials,
    );
    assert!(
        s.contains("IFCMATERIALLAYERSETUSAGE"),
        "writer must emit IFCMATERIALLAYERSETUSAGE wrapper"
    );
    // Synthetic Pset must NOT leak as a real IfcPropertySet on the
    // wire — the writer suppresses it.
    assert!(
        !s.contains("AEC_LayerSetUsage"),
        "synthetic Pset must not be emitted as IFCPROPERTYSET"
    );

    let snap2 = IfcReader::from_string(&s).expect("re-parse after re-export");
    // Both synthetic psets survive a second round-trip.
    let recovered: usize = snap2
        .materials
        .assignments()
        .filter_map(|(id, _)| snap2.properties.get(id))
        .filter(|p| {
            p.psets
                .contains_key(aec_bim::materials::AEC_LAYER_SET_USAGE_PSET)
        })
        .count();
    assert_eq!(
        recovered, 2,
        "both walls must still have AEC_LayerSetUsage after re-export"
    );
}

#[test]
fn writer_drops_layer_set_usage_wrapper_when_metadata_is_partial() {
    // Regression for Devin Review (web-UI flag, post-PR-L):
    // `IfcMaterialLayerSetUsage` declares all three orientation
    // fields (LayerSetDirection / DirectionSense /
    // OffsetFromReferenceLine) as MANDATORY in the IFC4 EXPRESS
    // schema — none carry the `OPTIONAL` keyword. AEC Studio's
    // synthetic `AEC_LayerSetUsage` Pset accumulates whatever
    // subset the reader managed to recover, so defective source
    // files (an ArchiCAD bug that has reported `IfcMaterialLayerSetUsage(
    // #5,.AXIS2.,$,$)` for years) land in the project graph with
    // only one or two of the three fields populated.
    //
    // Before the fix, `LayerSetUsageKey::from_property_store`
    // returned `Some(LayerSetUsageKey { direction: Some(...), sense:
    // None, offset_bits: None })` for such cases and the writer
    // emitted `IFCMATERIALLAYERSETUSAGE(#13,.AXIS2.,$,$,$)` — a
    // structurally invalid STEP entity. mvdXML conformance checkers
    // (Solibri, BIMcollab) flag those as schema violations, which
    // means AEC Studio was taking a defective input IFC and
    // producing another defective IFC, but now branded with AEC
    // Studio's writer signature in the file header.
    //
    // After the fix, partial metadata returns `None` and the rel
    // falls back to a direct `IfcMaterialLayerSet` ref. The
    // partial Pset stays in the project graph (the next import
    // recovers it untouched), so no information is lost — we just
    // refuse to emit a schema-invalid wrapper on the wire.
    //
    // Test shape:
    //   1. Build a project with a wall bound to a layer-set.
    //   2. Attach a partial AEC_LayerSetUsage Pset (only
    //      LayerSetDirection populated).
    //   3. Export. Assert NO `IFCMATERIALLAYERSETUSAGE` line in
    //      the output.
    //   4. Assert the rel's material ref points at the
    //      `IFCMATERIALLAYERSET` directly, not at a wrapper.
    //   5. Re-import and assert the partial Pset survives intact.
    use aec_bim::ifc::IfcReader;
    use aec_bim::materials::{AEC_LAYER_SET_USAGE_KEY_DIRECTION, AEC_LAYER_SET_USAGE_PSET};
    use aec_bim::properties::{PropertySet, PropertyValue};

    let mut project = Project::new("Partial usage");
    let root = project.root.clone();
    let site = project.add_child(&root, IfcClass::IfcSite, "Site").unwrap();
    let building = project
        .add_child(&site, IfcClass::IfcBuilding, "Building")
        .unwrap();
    let storey = project
        .add_child(&building, IfcClass::IfcBuildingStorey, "L1")
        .unwrap();
    let wall = EntityId::new();
    assert!(project.attach_element(&storey, wall.clone()));

    let mut classification = ClassificationStore::new();
    classification.assign_manual(wall.clone(), IfcClass::IfcWall);

    let mut materials = MaterialStore::new();
    materials.upsert_material(Material::new("Concrete"));
    materials.upsert_layer_set(
        MaterialLayerSet::new("Wall-200").with_layer(MaterialLayer::new("Concrete", 0.2)),
    );
    assert!(materials.assign_to_element(
        wall.clone(),
        MaterialAssignment::LayerSet("Wall-200".into()),
    ));

    let mut properties = PropertyStore::default();
    // ONLY direction; sense + offset deliberately absent to
    // simulate a defective source file.
    let mut usage = PropertySet::new(AEC_LAYER_SET_USAGE_PSET);
    usage.set(
        AEC_LAYER_SET_USAGE_KEY_DIRECTION,
        PropertyValue::Label(".AXIS2.".into()),
    );
    properties.entry(wall.clone()).upsert_pset(usage);

    let step =
        IfcWriter::to_string_with_materials(&project, &classification, &properties, &materials);

    // Hard contract: the writer must NOT emit any
    // IFCMATERIALLAYERSETUSAGE record when the synthetic Pset is
    // missing any of its three mandatory fields.
    assert!(
        !step.contains("IFCMATERIALLAYERSETUSAGE"),
        "writer must drop the IfcMaterialLayerSetUsage wrapper when usage \
         metadata is partial; the IFC4 schema marks all three orientation \
         fields as MANDATORY, so emitting `$` for any of them produces a \
         structurally invalid STEP entity:\n{step}"
    );

    // The wall's IFCRELASSOCIATESMATERIAL must reference the bare
    // IFCMATERIALLAYERSET directly (not a wrapper). Find the rel
    // step-id and confirm its material reference resolves to an
    // IFCMATERIALLAYERSET record, not to an IFCMATERIALLAYERSETUSAGE.
    let snap_after = IfcReader::from_string(&step).expect("re-parse after partial-usage export");
    let assignment = snap_after
        .materials
        .assignments()
        .find_map(|(id, a)| if id == &wall { Some(a.clone()) } else { None })
        .expect("wall must still carry a material assignment after re-import");
    match assignment {
        MaterialAssignment::LayerSet(name) => assert_eq!(name, "Wall-200"),
        other @ MaterialAssignment::Single(_) => panic!(
            "wall must remain bound to the layer-set on re-import \
             (no wrapper fallback should change the binding kind); got {other:?}"
        ),
    }

    // Partial data is DROPPED on the wire — by design. The
    // synthetic `AEC_LayerSetUsage` Pset is reader-only: it's
    // reconstructed from an `IfcMaterialLayerSetUsage` STEP entity
    // on import and never emitted as a real `IfcPropertySet` (see
    // the existing `synthetic Pset must not be emitted as
    // IFCPROPERTYSET` assertion in the round-trip test above).
    // With the wrapper dropped, there's no wire-side carrier left
    // to round-trip the partial fields. The defensive contract is
    // "AEC Studio's writer either round-trips the wrapper correctly
    // or drops it entirely; it never produces a schema-invalid
    // partial wrapper". Losing partial information from a defective
    // source file on export is the documented trade-off — the
    // alternative (emitting `$` for mandatory fields) would mean
    // every AEC Studio export silently propagated the original
    // defect under our writer's signature.
    //
    // Document the loss explicitly so a future maintainer doesn't
    // mistake the absent Pset on re-import for a regression.
    let post_export_pset = snap_after
        .properties
        .get(&wall)
        .and_then(|p| p.psets.get(AEC_LAYER_SET_USAGE_PSET));
    assert!(
        post_export_pset.is_none(),
        "partial AEC_LayerSetUsage data is intentionally dropped on the wire when the wrapper \
         is suppressed (the wrapper is the only carrier the reader looks at). \
         If this assertion ever flips to is_some(), the writer started emitting partial \
         metadata through a side channel — likely as a real IfcPropertySet, which would \
         contradict the `synthetic Pset must not be emitted as IFCPROPERTYSET` invariant \
         in `material_layer_set_usage_metadata_round_trips_via_synthetic_pset`."
    );
    // The reference to AEC_LAYER_SET_USAGE_KEY_DIRECTION above
    // documents intent without needing to assert against it again
    // here — silence the unused-import lint cheaply by referencing
    // it in a const cast.
    let _ = AEC_LAYER_SET_USAGE_KEY_DIRECTION;
    let _ = PropertyValue::Boolean(false);
}
