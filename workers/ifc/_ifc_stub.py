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
    def IsDefinedBy(self) -> list["_RelDefinesByProperties"]:
        return [
            rel
            for rel in _State.current.rels
            if rel.kind == "IfcRelDefinesByProperties"
            and self in rel.related_objects
        ]

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
    relating_property_definition: _PropertySet

    # PascalCase aliases for parity with real ifcopenshell.
    @property
    def RelatedObjects(self) -> list[_Entity]:
        return self.related_objects

    @property
    def RelatingPropertyDefinition(self) -> _PropertySet:
        return self.relating_property_definition


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

    def __init__(self) -> None:
        self.entities: list[_Entity] = []
        self.rels: list[Any] = []
        self._next_id = 1

    def activate(self) -> None:
        _State.current = self

    @property
    def schema(self) -> str:
        return "IFC4"

    # ----- create helpers -----

    def create_entity(self, type: str, **attrs: Any) -> _Entity:
        eid = self._next_id
        self._next_id += 1
        if "GlobalId" not in attrs:
            attrs["GlobalId"] = uuid.uuid4().hex
        e = _Entity(id=eid, type=type, attributes=dict(attrs))
        self.entities.append(e)
        return e

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
        else:  # pragma: no cover — we only use the three kinds above
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
            "entities": [
                {"id": e.id, "type": e.type, "attributes": e.attributes}
                for e in self.entities
            ],
            "rels": [self._serialise_rel(r) for r in self.rels],
        }
        with builtins.open(path, "w", encoding="utf-8") as f:
            json.dump(payload, f, indent=2)

    @classmethod
    def read(cls, path: str) -> "IfcStubFile":
        f = cls()
        with builtins.open(path, "r", encoding="utf-8") as fh:
            raw = json.load(fh)
        id_to_entity: dict[int, _Entity] = {}
        for entity_data in raw["entities"]:
            e = _Entity(
                id=entity_data["id"],
                type=entity_data["type"],
                attributes=dict(entity_data["attributes"]),
            )
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
                "relating_property_definition": {
                    "name": rel.relating_property_definition.name,
                    "properties": rel.relating_property_definition.properties,
                },
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
            pset_data = rel["relating_property_definition"]
            pset = _PropertySet(name=pset_data["name"], properties=pset_data["properties"])
            f.rels.append(
                _RelDefinesByProperties(
                    kind=kind,
                    related_objects=[id_to_entity[i] for i in rel["related_objects"]],
                    relating_property_definition=pset,
                )
            )


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
