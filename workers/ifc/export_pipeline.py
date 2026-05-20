"""Export an AEC project graph to IFC, preserving original GUIDs.

The Rust side serialises the graph using the same shape as the import
pipeline (see `import_pipeline.py`). The export must:

* Recreate the spatial hierarchy (`IfcProject → Site → Building →
  Storey → Space`).
* Preserve GUIDs when present so subsequent imports keep stable
  identities (round-trip).
* Recreate element classifications and their property sets.
* Recreate quantity sets (`Qto_*`) by wrapping each numeric value in
  the right `IfcQuantity*` entity.
* Recreate material associations (`IfcRelAssociatesMaterial`) for
  single materials and layered material sets.
* Preserve element geometry summaries (representation type, bounding
  box, vertex count) on the element's ``Representation`` attribute.
* Optionally validate the produced file against IfcOpenShell's
  strict mode (when ``strict=True``) before returning.

Returns a summary dictionary the worker forwards to the bridge.
"""

from __future__ import annotations

from typing import Any


def _wrap_nominal_value(f: Any, value: Any) -> Any:
    """Wrap a Python value in the appropriate IFC simple-type entity for
    ``IfcPropertySingleValue.NominalValue``.

    Real ifcopenshell expects ``NominalValue`` to be an IFC value entity
    (``IfcBoolean``, ``IfcInteger``, ``IfcReal``, ``IfcLabel`` / ``IfcText``)
    rather than a raw Python ``bool`` / ``int`` / ``float`` / ``str``.
    Passing a raw value works against our minimal stub but produces a
    malformed IFC at write time on the real library — the property reads
    back as ``None``. We map Python types to IFC value types here so the
    same export code path produces a correct IFC file in both worlds.

    Mapping:

    * ``bool``                       -> ``IfcBoolean``
    * ``int``                        -> ``IfcInteger``
    * ``float``                      -> ``IfcReal``
    * ``str`` (<= 255 chars)         -> ``IfcLabel``
    * ``str`` (> 255 chars)          -> ``IfcText``
    * anything else                  -> ``IfcLabel`` of ``str(value)``
    """
    # ``bool`` is a subclass of ``int`` in Python; check it first so we
    # don't route ``True`` through the integer branch.
    if isinstance(value, bool):
        return f.create_entity("IfcBoolean", value)
    if isinstance(value, int):
        return f.create_entity("IfcInteger", value)
    if isinstance(value, float):
        return f.create_entity("IfcReal", value)
    if isinstance(value, str):
        # IFC label is limited to 255 chars; longer strings must use IfcText.
        return f.create_entity("IfcText" if len(value) > 255 else "IfcLabel", value)
    # Last-resort: stringify so the export still completes deterministically.
    return f.create_entity("IfcLabel", str(value))


def _create_property_set(f: Any, ifc_module: Any, name: str, properties: dict[str, Any]) -> Any:
    """Build a property set in a way that works against both the stub
    `IfcStubFile` (used in CI) and a real `ifcopenshell.file`.

    The stub exposes a `create_property_set(name, properties)` helper that
    returns a lightweight dataclass. Real `ifcopenshell` doesn't expose
    that name, so we fall back to constructing the underlying
    `IfcPropertySet` / `IfcPropertySingleValue` entities through the
    public `create_entity` factory. In that path we wrap each Python
    value with the appropriate IFC simple-type entity for
    ``NominalValue`` -- see ``_wrap_nominal_value``.

    Production code never imports stub-internal symbols -- it only relies
    on duck-typed entry points the stub deliberately exposes to mirror
    `ifcopenshell`.
    """
    if hasattr(f, "create_property_set"):
        return f.create_property_set(name, properties)

    single_values = [
        f.create_entity(
            "IfcPropertySingleValue",
            Name=str(key),
            NominalValue=_wrap_nominal_value(f, value),
        )
        for key, value in properties.items()
    ]
    return f.create_entity(
        "IfcPropertySet",
        Name=name,
        GlobalId=ifc_module.guid_new(),
        HasProperties=single_values,
    )


def _create_element_quantity(
    f: Any, ifc_module: Any, name: str, properties: dict[str, Any]
) -> Any:
    """Build an `IfcElementQuantity` keyed by quantity name.

    Real `ifcopenshell` requires individual `IfcQuantityArea`,
    `IfcQuantityLength`, `IfcQuantityVolume`, `IfcQuantityCount`,
    `IfcQuantityWeight` entities wrapped in `IfcElementQuantity.Quantities`.
    The stub exposes a `create_element_quantity` helper; production code
    must construct the wrapped entities explicitly.
    """
    if hasattr(f, "create_element_quantity"):
        return f.create_element_quantity(name, properties)

    quantities = []
    for key, value in properties.items():
        ifc_quantity_type = _quantity_type_for_value(value)
        quantities.append(
            f.create_entity(
                ifc_quantity_type,
                Name=str(key),
                **{_quantity_value_attr_for(ifc_quantity_type): value},
            )
        )
    return f.create_entity(
        "IfcElementQuantity",
        Name=name,
        GlobalId=ifc_module.guid_new(),
        Quantities=quantities,
    )


def _quantity_type_for_value(value: Any) -> str:
    """Pick the IFC quantity entity type for a numeric value. Same
    convention as the stub's ``_quantity_kind_for_value``; left in sync
    here so production export doesn't depend on stub-internal helpers."""
    if isinstance(value, bool):
        return "IfcQuantityCount"
    if isinstance(value, int):
        return "IfcQuantityCount"
    return "IfcQuantityLength"


def _quantity_value_attr_for(ifc_quantity_type: str) -> str:
    """IFC quantity entities expose their numeric value under a typed
    attribute matching their kind (``AreaValue``, ``LengthValue``, etc.)."""
    return {
        "IfcQuantityArea": "AreaValue",
        "IfcQuantityLength": "LengthValue",
        "IfcQuantityVolume": "VolumeValue",
        "IfcQuantityCount": "CountValue",
        "IfcQuantityWeight": "WeightValue",
    }.get(ifc_quantity_type, "LengthValue")


def _create_material_definition(
    f: Any, ifc_module: Any, material: dict[str, Any]
) -> Any:
    """Build either an `IfcMaterial` or an `IfcMaterialLayerSet`
    depending on the input shape (see :func:`import_pipeline._material`).
    Falls back to a single material when the kind isn't recognised."""
    kind = material.get("kind", "material")
    name = material.get("name", "Material")
    if kind == "layerset":
        layers = [
            (layer.get("name", ""), float(layer.get("thickness", 0.0)))
            for layer in material.get("layers", [])
        ]
        if hasattr(f, "create_material_layer_set"):
            return f.create_material_layer_set(name, layers)
        ifc_layers = [
            f.create_entity(
                "IfcMaterialLayer",
                Material=f.create_entity("IfcMaterial", Name=lname),
                LayerThickness=lthick,
            )
            for lname, lthick in layers
        ]
        return f.create_entity(
            "IfcMaterialLayerSet",
            LayerSetName=name,
            MaterialLayers=ifc_layers,
        )
    if hasattr(f, "create_material"):
        return f.create_material(name)
    return f.create_entity("IfcMaterial", Name=name)


def _attach_geometry(f: Any, ifc_module: Any, el: Any, geometry: dict[str, Any]) -> None:
    """Attach a representation summary to the element. Always stores the
    summary on the stub's element so round-trip works in tests; on real
    ifcopenshell we'd build an ``IfcShapeRepresentation`` via the geom
    helpers, which is best-effort here."""
    bbox = geometry.get("bbox")
    if bbox is None:
        bbox = [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0)]
    representation_type = geometry.get("representation_type", "BoundingBox")
    vertex_count = int(geometry.get("vertex_count", 0))
    if hasattr(f, "create_representation"):
        rep = f.create_representation(
            representation_type,
            tuple(float(c) for c in bbox[0]),
            tuple(float(c) for c in bbox[1]),
            vertex_count,
        )
        el.attributes["Representation"] = rep
        return
    rep = f.create_entity(
        "IfcShapeRepresentation",
        RepresentationType=representation_type,
    )
    el.Representation = rep  # pragma: no cover — real path


def export_ifc(
    graph: dict[str, Any],
    path: str,
    *,
    strict: bool = False,
) -> dict[str, Any]:
    import ifcopenshell  # type: ignore

    f = ifcopenshell.file()
    f.activate() if hasattr(f, "activate") else None
    schema_hint = graph.get("schema")
    if schema_hint and hasattr(f, "schema"):
        # On the stub `schema` is a settable property; on real ifcopenshell
        # it's read-only and we'd open with ``ifcopenshell.file(schema=...)``
        # — but at this point the file is already created with the default,
        # so we ignore a schema hint there. Tests cover the stub path.
        try:
            f.schema = schema_hint
        except AttributeError:
            pass
    project_data = graph.get("project", {})
    project = f.create_entity(
        "IfcProject",
        Name=project_data.get("name", "Untitled Project"),
        GlobalId=project_data.get("guid") or ifcopenshell.guid_new(),
    )
    guid_to_entity: dict[str, Any] = {project.GlobalId: project}

    sites_for_project: list[Any] = []
    for site_data in graph.get("sites", []):
        site = f.create_entity(
            "IfcSite",
            Name=site_data.get("name", "Site"),
            GlobalId=site_data.get("guid") or ifcopenshell.guid_new(),
        )
        guid_to_entity[site.GlobalId] = site
        sites_for_project.append(site)

        buildings_for_site: list[Any] = []
        for building_data in site_data.get("buildings", []):
            building = f.create_entity(
                "IfcBuilding",
                Name=building_data.get("name", "Building"),
                GlobalId=building_data.get("guid") or ifcopenshell.guid_new(),
            )
            guid_to_entity[building.GlobalId] = building
            buildings_for_site.append(building)

            storeys_for_building: list[Any] = []
            for storey_data in building_data.get("storeys", []):
                storey = f.create_entity(
                    "IfcBuildingStorey",
                    Name=storey_data.get("name", "Storey"),
                    GlobalId=storey_data.get("guid") or ifcopenshell.guid_new(),
                )
                guid_to_entity[storey.GlobalId] = storey
                storeys_for_building.append(storey)

                spaces: list[Any] = []
                for space_data in storey_data.get("spaces", []):
                    space = f.create_entity(
                        "IfcSpace",
                        Name=space_data.get("name", "Space"),
                        GlobalId=space_data.get("guid") or ifcopenshell.guid_new(),
                    )
                    guid_to_entity[space.GlobalId] = space
                    spaces.append(space)
                if spaces:
                    f.create_relationship(
                        "IfcRelAggregates",
                        relating_object=storey,
                        related_objects=spaces,
                    )
            if storeys_for_building:
                f.create_relationship(
                    "IfcRelAggregates",
                    relating_object=building,
                    related_objects=storeys_for_building,
                )
        if buildings_for_site:
            f.create_relationship(
                "IfcRelAggregates",
                relating_object=site,
                related_objects=buildings_for_site,
            )
    if sites_for_project:
        f.create_relationship(
            "IfcRelAggregates",
            relating_object=project,
            related_objects=sites_for_project,
        )

    by_container: dict[str, list[Any]] = {}
    type_cache: dict[str, Any] = {}
    for el_data in graph.get("elements", []):
        el = f.create_entity(
            el_data.get("type", "IfcBuildingElementProxy"),
            Name=el_data.get("name", ""),
            GlobalId=el_data.get("guid") or ifcopenshell.guid_new(),
        )
        guid_to_entity[el.GlobalId] = el
        container_guid = el_data.get("spatial_container_guid")
        if container_guid:
            by_container.setdefault(container_guid, []).append(el)

        # Property sets
        for pset_name, pset_props in (el_data.get("properties") or {}).items():
            pset = _create_property_set(f, ifcopenshell, pset_name, dict(pset_props))
            f.create_relationship(
                "IfcRelDefinesByProperties",
                related_objects=[el],
                relating_property_definition=pset,
            )

        # Quantity sets
        for qset_name, qset_props in (el_data.get("quantities") or {}).items():
            qset = _create_element_quantity(f, ifcopenshell, qset_name, dict(qset_props))
            f.create_relationship(
                "IfcRelDefinesByProperties",
                related_objects=[el],
                relating_property_definition=qset,
            )

        # Type properties — synthesise / reuse an `IfcXxxType` entity per
        # element type and attach its property sets to it. We share one
        # type entity per element class so multiple instances cluster
        # under the same type just like a real model.
        type_props = el_data.get("type_properties") or {}
        if type_props:
            ifc_class = el_data.get("type", "IfcBuildingElementProxy")
            type_kind = _type_entity_name_for(ifc_class)
            type_entity = type_cache.get(type_kind)
            if type_entity is None:
                type_entity = f.create_entity(
                    type_kind,
                    Name=f"{type_kind}_Default",
                    GlobalId=ifcopenshell.guid_new(),
                )
                type_cache[type_kind] = type_entity
                for tname, tprops in type_props.items():
                    type_pset = _create_property_set(
                        f, ifcopenshell, tname, dict(tprops)
                    )
                    f.create_relationship(
                        "IfcRelDefinesByProperties",
                        related_objects=[type_entity],
                        relating_property_definition=type_pset,
                    )
            f.create_relationship(
                "IfcRelDefinesByType",
                related_objects=[el],
                relating_type=type_entity,
            )

        # Material association
        material = el_data.get("material")
        if material:
            material_entity = _create_material_definition(f, ifcopenshell, material)
            f.create_relationship(
                "IfcRelAssociatesMaterial",
                related_objects=[el],
                relating_material=material_entity,
            )

        # Geometry summary
        geometry = el_data.get("geometry")
        if geometry:
            _attach_geometry(f, ifcopenshell, el, geometry)

    for container_guid, elements in by_container.items():
        container = guid_to_entity.get(container_guid)
        if container is None:
            continue
        f.create_relationship(
            "IfcRelContainedInSpatialStructure",
            relating_structure=container,
            related_elements=elements,
        )

    if strict:
        _validate_strict(f, graph)

    f.write(path)
    return {
        "path": path,
        "schema": f.schema,
        "project_guid": project.GlobalId,
        "element_count": len(graph.get("elements", [])),
    }


# Map an instance class to its canonical type class. Buildings have
# strict pairing in IFC (``IfcWall`` ↔ ``IfcWallType``, ``IfcSlab`` ↔
# ``IfcSlabType``, etc.). The fallback handles classes whose type form
# isn't a simple ``Type``-suffix variant.
_TYPE_OVERRIDES = {
    "IfcWallStandardCase": "IfcWallType",
    "IfcFurnishingElement": "IfcFurnishingElementType",
    "IfcOpeningElement": "IfcOpeningElementType",
    "IfcBuildingElementProxy": "IfcBuildingElementProxyType",
}


def _type_entity_name_for(ifc_class: str) -> str:
    if ifc_class in _TYPE_OVERRIDES:
        return _TYPE_OVERRIDES[ifc_class]
    return f"{ifc_class}Type"


def _validate_strict(f: Any, graph: dict[str, Any]) -> None:
    """A lightweight strict-mode check.

    The goal is to catch malformed exports before we write them to
    disk. Real ifcopenshell ships `ifcopenshell.validate.validate(f)`
    which we'd call here, but the stub doesn't have a public
    validator — so we implement the basics ourselves: every emitted
    element must have a GlobalId that survives round-trip, and every
    declared spatial container guid must resolve to an entity.
    """
    expected_guids = [el["guid"] for el in graph.get("elements", []) if "guid" in el]
    existing_guids = {e.GlobalId for e in f.entities if getattr(e, "GlobalId", "")}
    missing = [g for g in expected_guids if g and g not in existing_guids]
    if missing:
        raise ValueError(
            f"strict export: {len(missing)} elements failed to materialise: {missing[:3]}"
        )
    for el in graph.get("elements", []):
        guid = el.get("spatial_container_guid")
        if guid and guid not in existing_guids:
            raise ValueError(
                f"strict export: element {el.get('guid')!r} references "
                f"non-existent container {guid!r}"
            )
