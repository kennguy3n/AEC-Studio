"""Round-trip tests for the IFC worker that use the in-process stub."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path


HERE = Path(__file__).parent
ROOT = HERE.parent
sys.path.insert(0, str(ROOT))


class IfcWorkerTests(unittest.TestCase):
    def setUp(self) -> None:
        from _ifc_stub import install_as_ifcopenshell  # type: ignore

        install_as_ifcopenshell()
        # Reload pipelines so they pick up the stubbed module.
        for m in ("import_pipeline", "export_pipeline", "validator", "aec_ifc_worker"):
            sys.modules.pop(m, None)

    def tearDown(self) -> None:
        from _ifc_stub import uninstall  # type: ignore

        uninstall()

    # ----- export/import roundtrip -----

    def test_export_then_import_preserves_hierarchy(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        graph = {
            "project": {"guid": "P_GUID", "name": "Test Project"},
            "sites": [
                {
                    "guid": "SITE_GUID",
                    "name": "Site 1",
                    "buildings": [
                        {
                            "guid": "BLD_GUID",
                            "name": "Building 1",
                            "storeys": [
                                {
                                    "guid": "ST_GUID",
                                    "name": "L01",
                                    "spaces": [{"guid": "SP1_GUID", "name": "Living"}],
                                }
                            ],
                        }
                    ],
                }
            ],
            "elements": [
                {
                    "guid": "WALL1",
                    "type": "IfcWall",
                    "name": "Wall_001",
                    "spatial_container_guid": "ST_GUID",
                    "properties": {
                        "Pset_WallCommon": {"FireRating": "F30", "LoadBearing": True}
                    },
                }
            ],
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            out = export_ifc(graph, path)
            self.assertEqual(out["element_count"], 1)
            self.assertEqual(out["project_guid"], "P_GUID")
            # Now import back and inspect.
            imported = import_ifc(path)
            self.assertEqual(imported["project"]["guid"], "P_GUID")
            self.assertEqual(imported["sites"][0]["guid"], "SITE_GUID")
            self.assertEqual(
                imported["sites"][0]["buildings"][0]["storeys"][0]["guid"], "ST_GUID"
            )
            self.assertEqual(len(imported["elements"]), 1)
            wall = imported["elements"][0]
            self.assertEqual(wall["type"], "IfcWall")
            self.assertEqual(wall["spatial_container_guid"], "ST_GUID")
            self.assertEqual(
                wall["properties"]["Pset_WallCommon"]["FireRating"], "F30"
            )

    # ----- validator -----

    def test_validator_flags_orphan_and_duplicate(self):
        from validator import validate  # type: ignore

        graph = {
            "project": {"guid": "P", "name": "X"},
            "sites": [
                {
                    "guid": "S",
                    "name": "Site",
                    "buildings": [
                        {
                            "guid": "B",
                            "name": "B1",
                            "storeys": [
                                {"guid": "ST", "name": "L1", "spaces": []}
                            ],
                        }
                    ],
                }
            ],
            "elements": [
                {"guid": "W1", "type": "IfcWall", "name": "w", "spatial_container_guid": "ST"},
                {"guid": "W1", "type": "IfcWall", "name": "dup", "spatial_container_guid": "ST"},
                {"guid": "W2", "type": "IfcWall", "name": "orphan", "spatial_container_guid": ""},
            ],
        }
        out = validate(graph)
        self.assertFalse(out["ok"])
        codes = sorted(e["code"] for e in out["errors"])
        self.assertIn("DUPLICATE_GUID", codes)
        self.assertIn("ORPHAN_ELEMENT", codes)
        # Storey without spaces is a warning.
        self.assertEqual(out["warnings"][0]["code"], "STOREY_NO_SPACES")

    # ----- dispatcher -----

    def test_dispatcher_ping_and_error(self):
        from aec_ifc_worker import dispatch  # type: ignore

        self.assertEqual(
            dispatch({"id": 1, "method": "ping"}),
            {"id": 1, "result": {"pong": True}},
        )
        out = dispatch({"id": 2, "method": "ifc.import", "params": {}})
        self.assertEqual(out["error"]["code"], "EXCEPTION")


    # ----- extended pipeline coverage -----

    def _full_graph(self):
        return {
            "schema": "IFC4",
            "project": {"guid": "P", "name": "Proj"},
            "sites": [
                {
                    "guid": "S",
                    "name": "Site",
                    "buildings": [
                        {
                            "guid": "B",
                            "name": "B1",
                            "storeys": [
                                {"guid": "ST", "name": "L1", "spaces": []}
                            ],
                        }
                    ],
                }
            ],
            "elements": [
                {
                    "guid": "W1",
                    "type": "IfcWall",
                    "name": "w",
                    "spatial_container_guid": "ST",
                    "properties": {
                        "Pset_WallCommon": {
                            "FireRating": "F60",
                            "LoadBearing": True,
                        }
                    },
                    "quantities": {
                        "Qto_WallBaseQuantities": {
                            "NetSideArea": 12.5,
                            "Length": 5.0,
                        }
                    },
                    "type_properties": {
                        "Pset_WallCommon": {"AcousticRating": "Rw45"}
                    },
                    "material": {
                        "kind": "layerset",
                        "name": "L_Wall_200",
                        "layers": [
                            {"name": "Plaster", "thickness": 12.5},
                            {"name": "Block", "thickness": 175.0},
                            {"name": "Plaster", "thickness": 12.5},
                        ],
                    },
                    "geometry": {
                        "representation_type": "SweptSolid",
                        "bbox": [[0, 0, 0], [5000, 200, 2400]],
                        "vertex_count": 24,
                    },
                }
            ],
        }

    def test_quantities_roundtrip(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(self._full_graph(), path)
            imported = import_ifc(path)
            wall = imported["elements"][0]
            self.assertEqual(
                wall["quantities"]["Qto_WallBaseQuantities"]["NetSideArea"], 12.5
            )
            self.assertEqual(
                wall["quantities"]["Qto_WallBaseQuantities"]["Length"], 5.0
            )
            # Quantities must not leak into the properties bucket.
            self.assertNotIn(
                "Qto_WallBaseQuantities", wall.get("properties", {})
            )

    def test_material_layerset_roundtrip(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(self._full_graph(), path)
            imported = import_ifc(path)
            material = imported["elements"][0]["material"]
            self.assertEqual(material["kind"], "layerset")
            self.assertEqual(material["name"], "L_Wall_200")
            self.assertEqual(len(material["layers"]), 3)
            self.assertEqual(material["layers"][1]["thickness"], 175.0)

    def test_type_properties_roundtrip(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(self._full_graph(), path)
            imported = import_ifc(path)
            type_props = imported["elements"][0]["type_properties"]
            self.assertEqual(
                type_props["Pset_WallCommon"]["AcousticRating"], "Rw45"
            )

    def test_geometry_summary_roundtrip(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(self._full_graph(), path)
            imported = import_ifc(path)
            geom = imported["elements"][0]["geometry"]
            self.assertEqual(geom["representation_type"], "SweptSolid")
            self.assertEqual(geom["bbox"], [[0.0, 0.0, 0.0], [5000.0, 200.0, 2400.0]])
            self.assertEqual(geom["vertex_count"], 24)

    def test_import_progress_callback_fires(self):
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        graph = self._full_graph()
        # Add many elements so the progress step actually fires.
        for i in range(64):
            graph["elements"].append(
                {
                    "guid": f"X{i}",
                    "type": "IfcWall",
                    "name": f"w{i}",
                    "spatial_container_guid": "ST",
                }
            )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(graph, path)
            updates: list[tuple[int, int]] = []
            import_ifc(path, progress=lambda done, total: updates.append((done, total)))
            self.assertGreater(len(updates), 1)
            # Final call always reports total == done.
            self.assertEqual(updates[-1][0], updates[-1][1])
            self.assertEqual(updates[-1][1], len(graph["elements"]))

    def test_extended_element_types_collected(self):
        """The importer must collect at least IfcCurtainWall, IfcRailing,
        IfcOpeningElement, IfcSanitaryTerminal — categories added in this
        pipeline extension."""
        from export_pipeline import export_ifc  # type: ignore
        from import_pipeline import import_ifc  # type: ignore

        graph = self._full_graph()
        extra = ["IfcCurtainWall", "IfcRailing", "IfcOpeningElement", "IfcSanitaryTerminal"]
        for kind in extra:
            graph["elements"].append(
                {
                    "guid": f"E_{kind}",
                    "type": kind,
                    "name": kind.lower(),
                    "spatial_container_guid": "ST",
                }
            )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            export_ifc(graph, path)
            imported = import_ifc(path)
            types = {el["type"] for el in imported["elements"]}
            for kind in extra:
                self.assertIn(kind, types)

    def test_strict_export_rejects_dangling_container(self):
        from export_pipeline import export_ifc  # type: ignore

        graph = self._full_graph()
        graph["elements"].append(
            {
                "guid": "ORPHAN",
                "type": "IfcWall",
                "name": "orphan",
                "spatial_container_guid": "DOES_NOT_EXIST",
            }
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            with self.assertRaises(ValueError):
                export_ifc(graph, path, strict=True)

    def test_strict_export_accepts_clean_graph(self):
        from export_pipeline import export_ifc  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "out.ifc.json")
            # Strict mode must succeed on a clean graph.
            out = export_ifc(self._full_graph(), path, strict=True)
            self.assertEqual(out["element_count"], 1)

    def test_schema_normalisation(self):
        from import_pipeline import _normalise_schema  # type: ignore

        self.assertEqual(_normalise_schema("IFC4"), "IFC4")
        self.assertEqual(_normalise_schema("Ifc4"), "IFC4")
        self.assertEqual(_normalise_schema("IFC4X3"), "IFC4X3")
        self.assertEqual(_normalise_schema("IFC2X3"), "IFC2X3")
        # Unknown but Ifc-prefixed: still returned as-is upper.
        self.assertEqual(_normalise_schema("IFC9"), "IFC9")


if __name__ == "__main__":
    unittest.main()
