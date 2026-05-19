"""Export an AEC project graph to IFC, preserving original GUIDs.

The Rust side serialises the graph using the same shape as the import
pipeline (see `import_pipeline.py`). The export must:

* Recreate the spatial hierarchy (`IfcProject → Site → Building →
  Storey → Space`).
* Preserve GUIDs when present so subsequent imports keep stable
  identities (round-trip).
* Recreate element classifications and their property sets.

Returns a summary dictionary the worker forwards to the bridge.
"""

from __future__ import annotations

from typing import Any


def export_ifc(graph: dict[str, Any], path: str) -> dict[str, Any]:
    import ifcopenshell  # type: ignore

    f = ifcopenshell.file()
    f.activate() if hasattr(f, "activate") else None
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
        for pset_name, pset_props in (el_data.get("properties") or {}).items():
            from _ifc_stub import _PropertySet  # type: ignore

            pset = _PropertySet(name=pset_name, properties=dict(pset_props))
            f.create_relationship(
                "IfcRelDefinesByProperties",
                related_objects=[el],
                relating_property_definition=pset,
            )

    for container_guid, elements in by_container.items():
        container = guid_to_entity.get(container_guid)
        if container is None:
            continue
        f.create_relationship(
            "IfcRelContainedInSpatialStructure",
            relating_structure=container,
            related_elements=elements,
        )

    f.write(path)
    return {
        "path": path,
        "schema": f.schema,
        "project_guid": project.GlobalId,
        "element_count": len(graph.get("elements", [])),
    }
