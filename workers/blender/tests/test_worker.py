"""End-to-end tests for the Blender worker that exercise the IPC
protocol, the dispatcher, and each handler — all with the bpy stub
installed (Blender is never required for CI).
"""

from __future__ import annotations

import io
import json
import sys
import unittest
from pathlib import Path


HERE = Path(__file__).parent
ROOT = HERE.parent
sys.path.insert(0, str(ROOT))


class BlenderWorkerTests(unittest.TestCase):
    def setUp(self) -> None:
        # Install the bpy stub before any worker module loads.
        from _bpy_stub import install_as_bpy  # type: ignore

        self.stub = install_as_bpy()
        # Force reimport of the worker modules so they pick up the stub.
        for m in [
            "aec_blender_worker",
            "scene_loader",
            "materials",
            "lighting",
            "eevee_preview",
            "cycles_final",
        ]:
            sys.modules.pop(m, None)

    def tearDown(self) -> None:
        from _bpy_stub import uninstall  # type: ignore

        uninstall()

    # ----- handlers -----

    def test_scene_load_creates_objects(self):
        from scene_loader import load_scene  # type: ignore

        scene = {
            "scene_name": "Scene",
            "units": "mm",
            "meshes": [
                {
                    "name": "Wall_001",
                    "vertices": [[0, 0, 0], [4500, 0, 0], [4500, 100, 0], [0, 100, 0]],
                    "faces": [[0, 1, 2, 3]],
                    "material": "wall_white",
                }
            ],
            "lights": [{"name": "Sun", "type": "SUN", "energy": 4.5}],
            "camera": {"name": "Cam", "location": [0, -5000, 1700], "focal_length_mm": 35},
        }
        result = load_scene(scene)
        self.assertEqual(result["mesh_count"], 1)
        self.assertEqual(result["light_count"], 1)
        self.assertEqual(result["meshes"], ["Wall_001"])
        # mm → m: 4500 mm became 4.5 m on the first vertex.
        import bpy  # type: ignore

        mesh = next(iter(bpy.data.meshes))
        self.assertAlmostEqual(mesh.vertices[1][0], 4.5, places=6)

    def test_materials_apply_creates_and_updates(self):
        from materials import apply_materials  # type: ignore

        out = apply_materials(
            [
                {"name": "wall_white", "albedo": [0.95, 0.94, 0.92], "metallic": 0.0, "roughness": 0.4},
                {"name": "wood_oak", "albedo": [0.55, 0.40, 0.25], "metallic": 0.0, "roughness": 0.6},
            ]
        )
        self.assertEqual(out["total"], 2)
        self.assertEqual(set(out["created"]), {"wall_white", "wood_oak"})
        # Second call should update, not duplicate.
        out2 = apply_materials([{"name": "wall_white"}])
        self.assertEqual(out2["updated"], ["wall_white"])

    def test_lighting_apply_creates_lights(self):
        from lighting import apply_lighting  # type: ignore

        out = apply_lighting(
            {
                "name": "warm_evening",
                "lights": [
                    {"name": "Sun", "type": "SUN", "energy": 0.6, "color": [1.0, 0.6, 0.3]},
                    {"name": "Fill", "type": "AREA", "energy": 200},
                ],
                "world": {"strength": 0.15},
            }
        )
        self.assertEqual(out["preset"], "warm_evening")
        self.assertEqual(out["lights"], ["Sun", "Fill"])

    def test_eevee_preview_invokes_render(self):
        from eevee_preview import render_eevee_preview  # type: ignore

        out = render_eevee_preview("/tmp/eevee.png", resolution_x=640, resolution_y=480)
        self.assertEqual(out["engine"], "eevee")
        self.assertTrue(out["output"].endswith("eevee.png"))
        self.assertEqual(self.stub.op_log[-1], {"op": "render", "write_still": True, "animation": False})

    def test_cycles_final_invokes_render_with_settings(self):
        from cycles_final import render_cycles_final  # type: ignore

        out = render_cycles_final(
            "/tmp/cycles.png", samples=256, resolution_x=320, resolution_y=240, denoise=True
        )
        self.assertEqual(out["engine"], "cycles")
        self.assertEqual(out["samples"], 256)
        import bpy  # type: ignore

        self.assertEqual(bpy.context.scene.cycles.samples, 256)
        self.assertEqual(bpy.context.scene.render.engine, "CYCLES")

    # ----- dispatcher / serve -----

    def test_dispatch_ping_returns_pong(self):
        from aec_blender_worker import dispatch  # type: ignore

        out = dispatch({"id": 1, "method": "ping"})
        self.assertEqual(out, {"id": 1, "result": {"pong": True}})

    def test_dispatch_unknown_method_returns_error(self):
        from aec_blender_worker import dispatch  # type: ignore

        out = dispatch({"id": 7, "method": "does.not.exist"})
        self.assertEqual(out["error"]["code"], "UNKNOWN_METHOD")

    def test_dispatch_handler_exception_returns_error_envelope(self):
        from aec_blender_worker import dispatch  # type: ignore

        # Pass a request that the eevee handler will refuse because it has
        # no 'output' key.
        out = dispatch({"id": 9, "method": "render.eevee_preview", "params": {}})
        self.assertEqual(out["error"]["code"], "EXCEPTION")
        self.assertIn("output", out["error"]["message"])

    def test_serve_processes_two_requests_then_shutdown(self):
        from aec_blender_worker import serve  # type: ignore

        sin = io.StringIO(
            "\n".join(
                [
                    json.dumps({"id": 1, "method": "ping"}),
                    json.dumps({"id": 2, "method": "ping"}),
                    json.dumps({"id": 3, "method": "shutdown"}),
                    "",
                ]
            )
        )
        sout = io.StringIO()
        exit_code = serve(stdin=sin, stdout=sout)
        self.assertEqual(exit_code, 0)
        lines = [json.loads(l) for l in sout.getvalue().strip().splitlines()]
        self.assertEqual(len(lines), 3)
        self.assertEqual(lines[0], {"id": 1, "result": {"pong": True}})
        self.assertEqual(lines[2]["result"], {"shutdown": True})

    def test_serve_handles_bad_json(self):
        from aec_blender_worker import serve  # type: ignore

        sin = io.StringIO("{this is not json}\n" + json.dumps({"id": 1, "method": "shutdown"}) + "\n")
        sout = io.StringIO()
        serve(stdin=sin, stdout=sout)
        lines = [json.loads(l) for l in sout.getvalue().strip().splitlines()]
        self.assertEqual(lines[0]["error"]["code"], "BAD_JSON")


if __name__ == "__main__":
    unittest.main()
