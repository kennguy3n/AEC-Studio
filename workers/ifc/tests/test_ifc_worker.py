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


if __name__ == "__main__":
    unittest.main()
