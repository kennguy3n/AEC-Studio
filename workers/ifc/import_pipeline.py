"""Import an IFC file into the AEC project graph.

The worker reads the file with ifcopenshell (or the stub), walks the
spatial hierarchy `IfcProject → IfcSite → IfcBuilding → IfcBuildingStorey
→ IfcSpace`, and emits a compact JSON document the Rust side can map
into `aec_bim` entities.

The returned dict has shape:

```json
{
  "schema": "IFC4",
  "project": {"guid": "...", "name": "..."},
  "sites": [{"guid": "...", "name": "...", "buildings": [...]}],
  "elements": [
    {"guid": "...", "type": "IfcWall", "name": "Wall_001",
     "spatial_container_guid": "...", "properties": {"Pset_WallCommon": {"FireRating": "F30"}}}
  ]
}
```
"""

from __future__ import annotations

from typing import Any


def import_ifc(path: str) -> dict[str, Any]:
    import ifcopenshell  # type: ignore

    f = ifcopenshell.open(path)
    f.activate() if hasattr(f, "activate") else None
    project = _first(f.by_type("IfcProject")) or {"GlobalId": "", "Name": ""}
    sites = [_serialise_site(site) for site in f.by_type("IfcSite")]
    elements = [_serialise_element(el) for el in _collect_elements(f)]
    return {
        "schema": f.schema,
        "project": {
            "guid": _gid(project),
            "name": _name(project),
        },
        "sites": sites,
        "elements": elements,
    }


_ELEMENT_TYPES = (
    "IfcWall",
    "IfcSlab",
    "IfcDoor",
    "IfcWindow",
    "IfcColumn",
    "IfcBeam",
    "IfcFurnishingElement",
    "IfcStair",
    "IfcRoof",
    "IfcCovering",
)


def _collect_elements(f: Any) -> list[Any]:
    out: list[Any] = []
    for kind in _ELEMENT_TYPES:
        out.extend(f.by_type(kind))
    return out


def _serialise_site(site: Any) -> dict[str, Any]:
    return {
        "guid": _gid(site),
        "name": _name(site),
        "buildings": [_serialise_building(b) for b in _aggregated(site)],
    }


def _serialise_building(building: Any) -> dict[str, Any]:
    return {
        "guid": _gid(building),
        "name": _name(building),
        "storeys": [_serialise_storey(s) for s in _aggregated(building)],
    }


def _serialise_storey(storey: Any) -> dict[str, Any]:
    spaces = [
        {"guid": _gid(s), "name": _name(s)}
        for s in _aggregated(storey)
        if hasattr(s, "is_a") and s.is_a("IfcSpace")
    ]
    contained = []
    if hasattr(storey, "ContainsElements"):
        for rel in storey.ContainsElements:
            for el in rel.related_elements:
                contained.append(_gid(el))
    return {
        "guid": _gid(storey),
        "name": _name(storey),
        "spaces": spaces,
        "contained_guids": contained,
    }


def _aggregated(entity: Any) -> list[Any]:
    if not hasattr(entity, "IsDecomposedBy"):
        return []
    out: list[Any] = []
    for rel in entity.IsDecomposedBy:
        out.extend(rel.related_objects)
    return out


def _serialise_element(el: Any) -> dict[str, Any]:
    container = _spatial_container(el)
    return {
        "guid": _gid(el),
        "type": _entity_type(el),
        "name": _name(el),
        "spatial_container_guid": _gid(container) if container is not None else None,
        "properties": _properties(el),
    }


def _entity_type(el: Any) -> str:
    # Real ifcopenshell: `el.is_a()` with no arg returns the type name. The
    # stub mirrors that. Fall back to the `.type` attribute if present.
    if hasattr(el, "is_a") and callable(el.is_a):
        try:
            value = el.is_a()
            if isinstance(value, str) and value:
                return value
        except TypeError:
            pass
    return str(getattr(el, "type", ""))


def _spatial_container(el: Any) -> Any | None:
    """Return the spatial structure that contains ``el`` (storey/space/site).

    Real ifcopenshell exposes the inverse relationship directly on the
    element as ``Element.ContainedInStructure``: a list of
    ``IfcRelContainedInSpatialStructure`` entities, each with a
    ``RelatingStructure`` attribute pointing at the container. We prefer
    that path because it works regardless of which file the element came
    from and does not rely on any module-level state.

    The in-process test stub (``_ifc_stub.py``) cannot easily synthesise
    the inverse on every element, so when ``ContainedInStructure`` is
    missing we fall back to walking ``IfcStubFile.rels`` via the stub's
    ``_State.current`` accessor.
    """
    # Real ifcopenshell path — every IFC entity carries inverse relationships.
    inverse = getattr(el, "ContainedInStructure", None)
    if inverse:
        first = inverse[0] if isinstance(inverse, (list, tuple)) else inverse
        relating = getattr(first, "RelatingStructure", None)
        if relating is None:
            relating = getattr(first, "relating_structure", None)
        if relating is not None:
            return relating

    # Stub fallback path — used when ifcopenshell elements don't carry the
    # inverse (e.g. our minimal `_ifc_stub`). The stub records rels on the
    # currently-open file via `_State.current`.
    import ifcopenshell  # type: ignore

    f = getattr(ifcopenshell, "_State", None)
    f = getattr(f, "current", None) if f is not None else None
    if f is None:
        return None
    for rel in getattr(f, "rels", []):
        if rel.kind == "IfcRelContainedInSpatialStructure" and el in rel.related_elements:
            return rel.relating_structure
    return None


def _properties(el: Any) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    if not hasattr(el, "IsDefinedBy"):
        return out
    for rel in el.IsDefinedBy:
        pset = getattr(rel, "relating_property_definition", None)
        if pset is None:
            continue
        out[pset.name] = dict(pset.properties)
    return out


def _first(seq: list[Any]) -> Any | None:
    return seq[0] if seq else None


def _gid(entity: Any) -> str:
    if entity is None:
        return ""
    if isinstance(entity, dict):
        return str(entity.get("GlobalId", ""))
    return str(getattr(entity, "GlobalId", ""))


def _name(entity: Any) -> str:
    if entity is None:
        return ""
    if isinstance(entity, dict):
        return str(entity.get("Name", ""))
    return str(getattr(entity, "Name", ""))
