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
     "spatial_container_guid": "...",
     "properties": {"Pset_WallCommon": {"FireRating": "F30"}},
     "quantities": {"Qto_WallBaseQuantities": {"NetSideArea": 12.5}},
     "type_properties": {"Pset_WallCommon": {"AcousticRating": "Rw45"}},
     "material": {"kind": "layerset", "name": "L_Wall",
                  "layers": [{"name": "Gypsum", "thickness": 12.5}]},
     "geometry": {"representation_type": "SweptSolid",
                  "bbox": [[0,0,0], [3000, 200, 2400]],
                  "vertex_count": 24}}
  ]
}
```

For large files the importer accepts a ``progress`` callback invoked
periodically with `(processed, total)` element counts so the desktop
UI can show a progress bar.
"""

from __future__ import annotations

from typing import Any, Callable


# IFC schema versions supported by the importer; emitted in the output's
# `schema` field for the Rust side to branch on if it ever needs to.
_SUPPORTED_SCHEMAS = ("IFC2X3", "IFC4", "IFC4X3")

ProgressCallback = Callable[[int, int], None]


def import_ifc(
    path: str,
    progress: ProgressCallback | None = None,
) -> dict[str, Any]:
    import ifcopenshell  # type: ignore

    f = ifcopenshell.open(path)
    f.activate() if hasattr(f, "activate") else None
    schema = _normalise_schema(f.schema)
    project = _first(f.by_type("IfcProject")) or {"GlobalId": "", "Name": ""}
    sites = [_serialise_site(site) for site in f.by_type("IfcSite")]
    raw_elements = _collect_elements(f)
    total = len(raw_elements)
    elements: list[dict[str, Any]] = []
    progress_step = max(1, total // 32)
    for index, el in enumerate(raw_elements):
        elements.append(_serialise_element(el))
        if progress is not None and (
            index + 1 == total or (index + 1) % progress_step == 0
        ):
            progress(index + 1, total)
    return {
        "schema": schema,
        "project": {
            "guid": _gid(project),
            "name": _name(project),
        },
        "sites": sites,
        "elements": elements,
    }


def _normalise_schema(schema: str) -> str:
    """Real ifcopenshell returns the schema name in upper case
    (`"IFC4"`, `"IFC4X3"`, `"IFC2X3"`). Some converters emit casing
    variations like `"Ifc4"`; normalise to upper-case canonical form,
    and warn (by emitting the raw schema unchanged) for any value not
    in our supported list — we don't fail the import because the
    pipeline is mostly schema-agnostic at the element level.
    """
    upper = schema.upper()
    if upper in _SUPPORTED_SCHEMAS:
        return upper
    return upper if upper.startswith("IFC") else schema


_ELEMENT_TYPES = (
    "IfcWall",
    "IfcWallStandardCase",
    "IfcSlab",
    "IfcDoor",
    "IfcWindow",
    "IfcColumn",
    "IfcBeam",
    "IfcCurtainWall",
    "IfcCovering",
    "IfcRailing",
    "IfcRoof",
    "IfcStair",
    "IfcFurniture",
    "IfcFurnishingElement",
    "IfcSanitaryTerminal",
    "IfcLightFixture",
    "IfcPlumbingFixture",
    "IfcOpeningElement",
    "IfcBuildingElementProxy",
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
            for el in _related_elements(rel):
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
        out.extend(_related_objects(rel))
    return out


def _serialise_element(el: Any) -> dict[str, Any]:
    container = _spatial_container(el)
    return {
        "guid": _gid(el),
        "type": _entity_type(el),
        "name": _name(el),
        "spatial_container_guid": _gid(container) if container is not None else None,
        "properties": _properties(el),
        "quantities": _quantities(el),
        "type_properties": _type_properties(el),
        "material": _material(el),
        "geometry": _geometry(el),
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
        if rel.kind == "IfcRelContainedInSpatialStructure" and el in _related_elements(rel):
            return rel.relating_structure
    return None


def _properties(el: Any) -> dict[str, dict[str, Any]]:
    """Extract `IfcPropertySet` bundles. Skips quantity sets (those are
    surfaced via :func:`_quantities`)."""
    out: dict[str, dict[str, Any]] = {}
    if not hasattr(el, "IsDefinedBy"):
        return out
    for rel in el.IsDefinedBy:
        if getattr(rel, "kind", "") == "IfcRelDefinesByType":
            continue
        pset = _relating_property_definition(rel)
        if pset is None or _is_quantity_set(pset):
            continue
        out[_name(pset)] = {
            k: v for k, v in (getattr(pset, "properties", {}) or {}).items()
            if not k.startswith("__")
        }
    return out


def _quantities(el: Any) -> dict[str, dict[str, Any]]:
    """Extract `IfcElementQuantity` bundles (the `Qto_*` family).

    Real ifcopenshell models each quantity as its own entity wrapped
    in an `IfcElementQuantity`; the stub flattens that into a
    properties dict. Either way we return a `{qset_name: {name: value}}`
    map, stripping any bookkeeping keys (those prefixed with `__`).
    """
    out: dict[str, dict[str, Any]] = {}
    if not hasattr(el, "IsDefinedBy"):
        return out
    for rel in el.IsDefinedBy:
        if getattr(rel, "kind", "") == "IfcRelDefinesByType":
            continue
        pset = _relating_property_definition(rel)
        if pset is None or not _is_quantity_set(pset):
            continue
        out[_name(pset)] = {
            k: v for k, v in (getattr(pset, "properties", {}) or {}).items()
            if not k.startswith("__")
        }
    return out


def _is_quantity_set(pset: Any) -> bool:
    """A property bundle is a quantity set if either (a) its name starts
    with the conventional ``Qto_`` prefix or (b) the stub tagged it as
    such via the bookkeeping key ``__quantities__``."""
    name = _name(pset)
    if name.startswith("Qto_"):
        return True
    props = getattr(pset, "properties", {}) or {}
    return "__quantities__" in props


def _type_properties(el: Any) -> dict[str, dict[str, Any]]:
    """Extract psets / quantities attached to the element's *type* (via
    ``IfcRelDefinesByType``). Type properties are inherited by instances
    in IFC and are the right place to store catalogue-level defaults."""
    out: dict[str, dict[str, Any]] = {}
    typed_by = getattr(el, "IsTypedBy", None) or []
    for rel in typed_by:
        type_entity = getattr(rel, "RelatingType", None) or getattr(rel, "relating_type", None)
        if type_entity is None or not hasattr(type_entity, "IsDefinedBy"):
            continue
        for type_rel in type_entity.IsDefinedBy:
            pset = _relating_property_definition(type_rel)
            if pset is None:
                continue
            out[_name(pset)] = {
                k: v for k, v in (getattr(pset, "properties", {}) or {}).items()
                if not k.startswith("__")
            }
    return out


def _material(el: Any) -> dict[str, Any] | None:
    """Extract the material association (`IfcRelAssociatesMaterial`).

    Returns either:
      * ``{"kind": "material", "name": "Concrete"}`` for a single material,
      * ``{"kind": "layerset", "name": "L_Wall",
          "layers": [{"name": "Gypsum", "thickness": 12.5}, ...]}`` for
        an ``IfcMaterialLayerSet`` (wall/slab/roof typical), or
      * ``None`` if no association exists.
    """
    associations = getattr(el, "HasAssociations", None) or []
    for rel in associations:
        material = getattr(rel, "RelatingMaterial", None) or getattr(
            rel, "relating_material", None
        )
        if material is None:
            continue
        layers = getattr(material, "MaterialLayers", None) or getattr(
            material, "material_layers", None
        )
        if layers:
            return {
                "kind": "layerset",
                "name": _name(material),
                "layers": [
                    {"name": _name(layer), "thickness": float(getattr(layer, "thickness", 0.0))}
                    for layer in layers
                ],
            }
        if hasattr(material, "Name") or hasattr(material, "name"):
            return {"kind": "material", "name": _name(material)}
    return None


def _geometry(el: Any) -> dict[str, Any] | None:
    """Extract a minimal geometry summary: representation type, axis-aligned
    bounding box, and vertex count.

    The full B-rep / swept solid stays in ifcopenshell; the Rust side only
    needs enough to render thumbnails, run validation, and quantity-take-off.
    """
    rep = getattr(el, "Representation", None)
    if rep is None:
        return None
    if hasattr(rep, "representation_type"):
        return {
            "representation_type": rep.representation_type,
            "bbox": [list(rep.bbox_min), list(rep.bbox_max)],
            "vertex_count": int(getattr(rep, "vertex_count", 0)),
        }
    # Real ifcopenshell: ``el.Representation`` is an ``IfcProductRepresentation``
    # whose ``Representations`` list contains ``IfcShapeRepresentation`` entries.
    # We don't run a geometry kernel inside the worker; bbox / vertex_count
    # extraction is deferred to the host's ifc.geom utility. We still emit
    # representation_type so the Rust side knows whether geometry is available.
    reps = getattr(rep, "Representations", None) or []
    if reps:
        first = reps[0]
        return {
            "representation_type": getattr(first, "RepresentationType", "")
            or _entity_type(first),
            "bbox": None,
            "vertex_count": 0,
        }
    return None


# --- Cross-version attribute accessors ---
#
# Real ifcopenshell exposes IFC attributes in the schema-canonical PascalCase
# (``rel.RelatedElements``, ``rel.RelatedObjects``,
# ``rel.RelatingPropertyDefinition``). Older versions and our in-process stub
# may also expose snake_case aliases (``related_elements`` etc.). The worker
# is responsible for being source-compatible with both, so all relationship
# attribute access goes through these helpers.


def _related_elements(rel: Any) -> list[Any]:
    value = getattr(rel, "RelatedElements", None)
    if value is None:
        value = getattr(rel, "related_elements", None)
    return list(value) if value is not None else []


def _related_objects(rel: Any) -> list[Any]:
    value = getattr(rel, "RelatedObjects", None)
    if value is None:
        value = getattr(rel, "related_objects", None)
    return list(value) if value is not None else []


def _relating_property_definition(rel: Any) -> Any | None:
    pset = getattr(rel, "RelatingPropertyDefinition", None)
    if pset is None:
        pset = getattr(rel, "relating_property_definition", None)
    return pset


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
