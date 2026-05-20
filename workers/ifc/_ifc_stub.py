"""In-process stand-in for `ifcopenshell` covering the slice of the
public API the AEC worker touches: file open/save, entity creation,
iteration, attribute access, and GUID generation.

Tests use this so CI machines without an `ifcopenshell` binary can still
exercise the worker's algorithms end-to-end.
"""

from __future__ import annotations

import builtins
import json
import uuid
from dataclasses import dataclass, field
from typing import Any, Iterable


@dataclass
class _Entity:
    id: int
    type: str
    attributes: dict[str, Any] = field(default_factory=dict)

    def is_a(self, kind: str | None = None) -> Any:
        """Match real ifcopenshell: no arg → return type string; arg → bool."""
        if kind is None:
            return self.type
        return self.type == kind

    @property
    def GlobalId(self) -> str:
        return str(self.attributes.get("GlobalId", ""))

    @property
    def Name(self) -> str:
        return str(self.attributes.get("Name", ""))

    @property
    def ContainsElements(self) -> list["_RelContains"]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelContainedInSpatialStructure"
            and rel.relating_structure is self
        ]

    @property
    def IsDecomposedBy(self) -> list["_RelAggregates"]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelAggregates" and rel.relating_object is self
        ]

    @property
    def IsDefinedBy(self) -> list[Any]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind in ("IfcRelDefinesByProperties", "IfcRelDefinesByType")
            and self in rel.related_objects
        ]

    @property
    def IsTypedBy(self) -> list["_RelDefinesByType"]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelDefinesByType" and self in rel.related_objects
        ]

    @property
    def HasAssociations(self) -> list["_RelAssociatesMaterial"]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelAssociatesMaterial"
            and self in rel.related_objects
        ]

    @property
    def Representation(self) -> Any:
        return self.attributes.get("Representation")

    @property
    def ContainedInStructure(self) -> list["_RelContains"]:
        """Inverse of `_RelContains` from the element side.

        Real ifcopenshell exposes this as the inverse attribute on every
        IFC entity; production code reads it to find the spatial container.
        Mirroring it here lets the import pipeline use a single code path
        regardless of whether the stub or the real library is in use.
        """
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelContainedInSpatialStructure"
            and self in rel.related_elements
        ]


@dataclass
class _PropertySet:
    name: str
    properties: dict[str, Any] = field(default_factory=dict)

    # Real ifcopenshell exposes the IFC attribute as PascalCase
    # ``Name``. Mirror it so production code that reads ``pset.Name`` works
    # against the stub too.
    @property
    def Name(self) -> str:
        return self.name


@dataclass
class _RelContains:
    kind: str
    relating_structure: _Entity
    related_elements: list[_Entity]

    # Real ifcopenshell exposes IFC attributes in PascalCase. We mirror
    # the few the worker reads so production code is identical across
    # the stub and the real library.
    @property
    def RelatingStructure(self) -> _Entity:
        return self.relating_structure

    @property
    def RelatedElements(self) -> list[_Entity]:
        return self.related_elements


@dataclass
class _RelAggregates:
    kind: str
    relating_object: _Entity
    related_objects: list[_Entity]

    # Mirror real ifcopenshell, which exposes IFC attributes in PascalCase
    # (the schema-canonical form). Production import code reads `.RelatingObject`
    # / `.RelatedObjects`; the stub exposes both spellings so the same code
    # path works against the stub in CI and the real library in production.
    @property
    def RelatingObject(self) -> _Entity:
        return self.relating_object

    @property
    def RelatedObjects(self) -> list[_Entity]:
        return self.related_objects


@dataclass
class _RelDefinesByProperties:
    kind: str
    related_objects: list[_Entity]
    relating_property_definition: Any  # _PropertySet or _ElementQuantity

    # PascalCase aliases for parity with real ifcopenshell.
    @property
    def RelatedObjects(self) -> list[_Entity]:
        return self.related_objects

    @property
    def RelatingPropertyDefinition(self) -> Any:
        return self.relating_property_definition


@dataclass
class _ElementQuantity:
    """Stub of `IfcElementQuantity` — a bundle of named numeric quantities
    (areas, lengths, volumes, counts) attached to one or more elements via
    `IfcRelDefinesByProperties`.

    Real ifcopenshell models each quantity as its own entity
    (`IfcQuantityArea`, `IfcQuantityLength`, …) wrapped in an
    `IfcElementQuantity`. The stub flattens that into a single dict keyed
    by quantity name, with the value's IFC unit kind captured as the
    quantity's discriminator inside `properties["__quantities__"]` so the
    exporter can route values through the right `IfcQuantity*` factory.
    """

    name: str
    properties: dict[str, Any] = field(default_factory=dict)

    @property
    def Name(self) -> str:
        return self.name


@dataclass
class _MaterialLayer:
    name: str
    thickness: float = 0.0

    @property
    def Name(self) -> str:
        return self.name

    @property
    def Material(self) -> Any:
        # Wrap into a dataclass with a Name so the importer can read it
        # the same way it reads a single material.
        return type("_Material", (), {"Name": self.name})()


@dataclass
class _Material:
    name: str

    @property
    def Name(self) -> str:
        return self.name

    # When something looks up the layer set on a single material the
    # stub returns the bare material.
    @property
    def ForLayerSet(self) -> Any:
        return None


@dataclass
class _MaterialLayerSet:
    name: str
    material_layers: list[_MaterialLayer] = field(default_factory=list)

    @property
    def Name(self) -> str:
        return self.name

    @property
    def MaterialLayers(self) -> list[_MaterialLayer]:
        return self.material_layers


@dataclass
class _RelDefinesByType:
    kind: str
    related_objects: list[_Entity]
    relating_type: _Entity

    @property
    def RelatedObjects(self) -> list[_Entity]:
        return self.related_objects

    @property
    def RelatingType(self) -> _Entity:
        return self.relating_type


@dataclass
class _RelAssociatesMaterial:
    kind: str
    related_objects: list[_Entity]
    relating_material: Any  # _Material | _MaterialLayerSet

    @property
    def RelatedObjects(self) -> list[_Entity]:
        return self.related_objects

    @property
    def RelatingMaterial(self) -> Any:
        return self.relating_material


@dataclass
class _Representation:
    """Minimal `IfcShapeRepresentation` analogue.

    The stub stores just the bounding-box extents so the importer can
    report a geometry summary (axis-aligned bbox + vertex count) without
    needing a real solids kernel. Production code reading a real
    `ifcopenshell.file` ignores this stub and uses
    `ifcopenshell.geom.create_shape` instead — both paths converge on
    the same JSON shape returned by `import_pipeline._geometry`.
    """

    representation_type: str  # e.g. "SweptSolid", "BoundingBox"
    bbox_min: tuple[float, float, float] = (0.0, 0.0, 0.0)
    bbox_max: tuple[float, float, float] = (0.0, 0.0, 0.0)
    vertex_count: int = 0

    @property
    def RepresentationType(self) -> str:
        return self.representation_type


def _quantity_kind_for_value(value: Any) -> str:
    """Pick the IFC quantity entity name that matches a numeric value's
    likely unit. We can't sniff units from a bare ``float`` so we route by
    convention: integers → ``IfcQuantityCount``, floats default to
    ``IfcQuantityLength``. Callers that need a different routing (areas,
    volumes, weights) should pass a string-prefixed key (``area:12.5``)
    or build the quantity by hand. This keeps the stub deterministic
    while leaving the door open for richer mapping later."""
    if isinstance(value, bool):
        return "IfcQuantityCount"
    if isinstance(value, int):
        return "IfcQuantityCount"
    if isinstance(value, float):
        return "IfcQuantityLength"
    return "IfcQuantityLength"


class _State:
    """Module-level current file (set by `IfcStubFile.activate`).

    `_Entity` accessors look it up to find their relationships. The state
    lives only for the duration of a single test.
    """

    current: "IfcStubFile" = None  # type: ignore[assignment]


class IfcStubFile:
    """Stub IFC file that records entities/relationships and can be
    serialised to JSON-on-disk so import/export round-trip without
    Blender or ifcopenshell.
    """

    def __init__(self, schema: str = "IFC4") -> None:
        self.entities: list[_Entity] = []
        self.rels: list[Any] = []
        self._next_id = 1
        self._schema = schema

    def activate(self) -> None:
        _State.current = self

    @property
    def schema(self) -> str:
        return self._schema

    @schema.setter
    def schema(self, value: str) -> None:
        self._schema = value

    # ----- create helpers -----

    def create_entity(self, type: str, **attrs: Any) -> _Entity:
        eid = self._next_id
        self._next_id += 1
        if "GlobalId" not in attrs:
            attrs["GlobalId"] = uuid.uuid4().hex
        e = _Entity(id=eid, type=type, attributes=dict(attrs))
        self.entities.append(e)
        return e

    def create_element_quantity(
        self, name: str, properties: dict[str, Any]
    ) -> _ElementQuantity:
        """Build an `IfcElementQuantity` bundle keyed by quantity name.

        `properties` is the externally-presented dict ({"NetSideArea":
        12.5, …}). For each value the stub records its IFC quantity
        kind alongside under `__quantities__` so the exporter can route
        through the correct `IfcQuantity*` factory; production code
        only reads `.properties` and never sees the bookkeeping.
        """
        kinds: dict[str, str] = {}
        for key, value in properties.items():
            kinds[key] = _quantity_kind_for_value(value)
        eq = _ElementQuantity(name=name, properties=dict(properties))
        eq.properties.setdefault("__quantities__", kinds)
        return eq

    def create_material(self, name: str) -> _Material:
        return _Material(name=name)

    def create_material_layer_set(
        self, name: str, layers: list[tuple[str, float]]
    ) -> _MaterialLayerSet:
        return _MaterialLayerSet(
            name=name,
            material_layers=[_MaterialLayer(name=n, thickness=t) for n, t in layers],
        )

    def create_representation(
        self,
        representation_type: str,
        bbox_min: tuple[float, float, float],
        bbox_max: tuple[float, float, float],
        vertex_count: int,
    ) -> _Representation:
        return _Representation(
            representation_type=representation_type,
            bbox_min=bbox_min,
            bbox_max=bbox_max,
            vertex_count=vertex_count,
        )

    def create_property_set(self, name: str, properties: dict[str, Any]) -> _PropertySet:
        """Build a property set bundle the worker can attach to elements
        via `create_relationship("IfcRelDefinesByProperties", ...)`.

        Real `ifcopenshell` returns an `IfcPropertySet` entity; the stub
        returns a lightweight dataclass with the same `.name` /
        `.properties` shape so production export code doesn't need to
        branch on type.
        """
        return _PropertySet(name=name, properties=dict(properties))

    def create_relationship(self, kind: str, **payload: Any) -> Any:
        if kind == "IfcRelContainedInSpatialStructure":
            rel = _RelContains(
                kind=kind,
                relating_structure=payload["relating_structure"],
                related_elements=list(payload["related_elements"]),
            )
        elif kind == "IfcRelAggregates":
            rel = _RelAggregates(
                kind=kind,
                relating_object=payload["relating_object"],
                related_objects=list(payload["related_objects"]),
            )
        elif kind == "IfcRelDefinesByProperties":
            rel = _RelDefinesByProperties(
                kind=kind,
                related_objects=list(payload["related_objects"]),
                relating_property_definition=payload["relating_property_definition"],
            )
        elif kind == "IfcRelDefinesByType":
            rel = _RelDefinesByType(
                kind=kind,
                related_objects=list(payload["related_objects"]),
                relating_type=payload["relating_type"],
            )
        elif kind == "IfcRelAssociatesMaterial":
            rel = _RelAssociatesMaterial(
                kind=kind,
                related_objects=list(payload["related_objects"]),
                relating_material=payload["relating_material"],
            )
        else:  # pragma: no cover — we only use the supported kinds above
            raise ValueError(f"unknown relationship kind: {kind}")
        self.rels.append(rel)
        return rel

    # ----- queries -----

    def by_type(self, kind: str) -> list[_Entity]:
        return [e for e in self.entities if e.type == kind]

    def by_guid(self, guid: str) -> _Entity | None:
        for e in self.entities:
            if e.GlobalId == guid:
                return e
        return None

    # ----- persistence (JSON-on-disk for tests) -----

    def write(self, path: str) -> None:
        payload = {
            "schema": self._schema,
            "entities": [
                {
                    "id": e.id,
                    "type": e.type,
                    "attributes": {
                        k: _attr_to_json(v) for k, v in e.attributes.items()
                    },
                }
                for e in self.entities
            ],
            "rels": [self._serialise_rel(r) for r in self.rels],
        }
        with builtins.open(path, "w", encoding="utf-8") as f:
            json.dump(payload, f, indent=2)

    @classmethod
    def read(cls, path: str) -> "IfcStubFile":
        with builtins.open(path, "r", encoding="utf-8") as fh:
            raw = json.load(fh)
        f = cls(schema=raw.get("schema", "IFC4"))
        id_to_entity: dict[int, _Entity] = {}
        for entity_data in raw["entities"]:
            attrs = {k: _json_to_attr(v) for k, v in entity_data["attributes"].items()}
            e = _Entity(id=entity_data["id"], type=entity_data["type"], attributes=attrs)
            f.entities.append(e)
            id_to_entity[e.id] = e
            f._next_id = max(f._next_id, e.id + 1)
        for rel in raw["rels"]:
            cls._deserialise_rel(f, rel, id_to_entity)
        return f

    @staticmethod
    def _serialise_rel(rel: Any) -> dict[str, Any]:
        if isinstance(rel, _RelContains):
            return {
                "kind": rel.kind,
                "relating_structure": rel.relating_structure.id,
                "related_elements": [e.id for e in rel.related_elements],
            }
        if isinstance(rel, _RelAggregates):
            return {
                "kind": rel.kind,
                "relating_object": rel.relating_object.id,
                "related_objects": [e.id for e in rel.related_objects],
            }
        if isinstance(rel, _RelDefinesByProperties):
            return {
                "kind": rel.kind,
                "related_objects": [e.id for e in rel.related_objects],
                "relating_property_definition": _serialise_property_definition(
                    rel.relating_property_definition
                ),
            }
        if isinstance(rel, _RelDefinesByType):
            return {
                "kind": rel.kind,
                "related_objects": [e.id for e in rel.related_objects],
                "relating_type": rel.relating_type.id,
            }
        if isinstance(rel, _RelAssociatesMaterial):
            return {
                "kind": rel.kind,
                "related_objects": [e.id for e in rel.related_objects],
                "relating_material": _serialise_material(rel.relating_material),
            }
        return {"kind": "unknown"}  # pragma: no cover

    @staticmethod
    def _deserialise_rel(
        f: "IfcStubFile", rel: dict[str, Any], id_to_entity: dict[int, _Entity]
    ) -> None:
        kind = rel["kind"]
        if kind == "IfcRelContainedInSpatialStructure":
            f.rels.append(
                _RelContains(
                    kind=kind,
                    relating_structure=id_to_entity[rel["relating_structure"]],
                    related_elements=[id_to_entity[i] for i in rel["related_elements"]],
                )
            )
        elif kind == "IfcRelAggregates":
            f.rels.append(
                _RelAggregates(
                    kind=kind,
                    relating_object=id_to_entity[rel["relating_object"]],
                    related_objects=[id_to_entity[i] for i in rel["related_objects"]],
                )
            )
        elif kind == "IfcRelDefinesByProperties":
            pset = _deserialise_property_definition(rel["relating_property_definition"])
            f.rels.append(
                _RelDefinesByProperties(
                    kind=kind,
                    related_objects=[id_to_entity[i] for i in rel["related_objects"]],
                    relating_property_definition=pset,
                )
            )
        elif kind == "IfcRelDefinesByType":
            f.rels.append(
                _RelDefinesByType(
                    kind=kind,
                    related_objects=[id_to_entity[i] for i in rel["related_objects"]],
                    relating_type=id_to_entity[rel["relating_type"]],
                )
            )
        elif kind == "IfcRelAssociatesMaterial":
            f.rels.append(
                _RelAssociatesMaterial(
                    kind=kind,
                    related_objects=[id_to_entity[i] for i in rel["related_objects"]],
                    relating_material=_deserialise_material(rel["relating_material"]),
                )
            )


def _serialise_property_definition(definition: Any) -> dict[str, Any]:
    """Tag whether a definition is a property set or a quantity set so the
    deserialiser can rebuild the right dataclass. We rely on duck-typing
    rather than ``isinstance`` so that mock objects in tests serialise too.
    """
    is_quantity = isinstance(definition, _ElementQuantity)
    return {
        "kind": "IfcElementQuantity" if is_quantity else "IfcPropertySet",
        "name": definition.name,
        "properties": dict(definition.properties),
    }


def _deserialise_property_definition(payload: dict[str, Any]) -> Any:
    if payload.get("kind") == "IfcElementQuantity":
        return _ElementQuantity(name=payload["name"], properties=dict(payload["properties"]))
    return _PropertySet(name=payload["name"], properties=dict(payload["properties"]))


def _serialise_material(material: Any) -> dict[str, Any]:
    if isinstance(material, _MaterialLayerSet):
        return {
            "kind": "IfcMaterialLayerSet",
            "name": material.name,
            "layers": [
                {"name": layer.name, "thickness": layer.thickness}
                for layer in material.material_layers
            ],
        }
    name = getattr(material, "Name", None) or getattr(material, "name", "")
    return {"kind": "IfcMaterial", "name": str(name)}


def _deserialise_material(payload: dict[str, Any]) -> Any:
    if payload.get("kind") == "IfcMaterialLayerSet":
        return _MaterialLayerSet(
            name=payload["name"],
            material_layers=[
                _MaterialLayer(name=ml["name"], thickness=float(ml["thickness"]))
                for ml in payload.get("layers", [])
            ],
        )
    return _Material(name=payload["name"])


def _attr_to_json(value: Any) -> Any:
    """Convert one entity attribute to a JSON-safe representation."""
    if isinstance(value, _Representation):
        return {
            "__type__": "_Representation",
            "representation_type": value.representation_type,
            "bbox_min": list(value.bbox_min),
            "bbox_max": list(value.bbox_max),
            "vertex_count": value.vertex_count,
        }
    return value


def _json_to_attr(value: Any) -> Any:
    if isinstance(value, dict) and value.get("__type__") == "_Representation":
        return _Representation(
            representation_type=value["representation_type"],
            bbox_min=tuple(value["bbox_min"]),
            bbox_max=tuple(value["bbox_max"]),
            vertex_count=value["vertex_count"],
        )
    return value


def open(path: str) -> IfcStubFile:
    """ifcopenshell-compatible `open(path)` helper."""
    return IfcStubFile.read(path)


def file() -> IfcStubFile:
    """ifcopenshell-compatible empty-file factory."""
    return IfcStubFile()


def guid_new() -> str:
    return uuid.uuid4().hex


def install_as_ifcopenshell() -> None:
    """Register this module as `ifcopenshell` so the worker code can
    `import ifcopenshell` without modification.
    """
    import sys

    sys.modules["ifcopenshell"] = sys.modules[__name__]


def uninstall() -> None:
    import sys

    sys.modules.pop("ifcopenshell", None)
