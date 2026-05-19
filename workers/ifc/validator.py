"""Validation pass over an imported IFC graph.

Catches:

* Duplicate GUIDs.
* Orphan elements (no spatial container).
* Empty/null GUIDs.
* Storeys without spaces (warning only).
"""

from __future__ import annotations

from collections import Counter
from typing import Any


def validate(graph: dict[str, Any]) -> dict[str, Any]:
    errors: list[dict[str, Any]] = []
    warnings: list[dict[str, Any]] = []
    seen: list[str] = []

    def walk_guid(guid: str, kind: str) -> None:
        if not guid:
            errors.append({"code": "EMPTY_GUID", "kind": kind})
            return
        seen.append(guid)

    project = graph.get("project", {})
    walk_guid(project.get("guid", ""), "IfcProject")
    for site in graph.get("sites", []):
        walk_guid(site.get("guid", ""), "IfcSite")
        for building in site.get("buildings", []):
            walk_guid(building.get("guid", ""), "IfcBuilding")
            for storey in building.get("storeys", []):
                walk_guid(storey.get("guid", ""), "IfcBuildingStorey")
                if not storey.get("spaces"):
                    warnings.append({"code": "STOREY_NO_SPACES", "guid": storey.get("guid")})
                for space in storey.get("spaces", []):
                    walk_guid(space.get("guid", ""), "IfcSpace")

    valid_containers = set(seen)
    for el in graph.get("elements", []):
        walk_guid(el.get("guid", ""), el.get("type", "IfcElement"))
        container = el.get("spatial_container_guid")
        if container is None or container == "":
            errors.append(
                {"code": "ORPHAN_ELEMENT", "guid": el.get("guid"), "type": el.get("type")}
            )
        elif container not in valid_containers:
            errors.append(
                {
                    "code": "DANGLING_CONTAINER",
                    "guid": el.get("guid"),
                    "container_guid": container,
                }
            )

    counts = Counter(seen)
    for guid, count in counts.items():
        if count > 1:
            errors.append({"code": "DUPLICATE_GUID", "guid": guid, "count": count})

    return {
        "ok": not errors,
        "errors": errors,
        "warnings": warnings,
        "guid_count": len(seen),
    }
