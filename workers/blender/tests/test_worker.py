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
            "panorama",
            "walkthrough",
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
        self.assertEqual(out["ies_attached"], [])
        self.assertEqual(out["world_strength"], 0.15)

    def test_lighting_apply_attaches_ies_profile(self):
        import tempfile
        from lighting import apply_lighting  # type: ignore

        ies_text = (
            "IESNA:LM-63-2002\n"
            "TILT=NONE\n"
            "1 1000.0 1.0 5 1 1 2 0.0 0.0 0.0\n"
            "1.0 1.0 100.0\n"
            "0.0 22.5 45.0 67.5 90.0\n"
            "0.0\n"
            "10.0 8.0 6.0 4.0 2.0\n"
        )
        with tempfile.NamedTemporaryFile(
            "w", suffix=".ies", delete=False
        ) as fh:
            fh.write(ies_text)
            ies_path = fh.name
        out = apply_lighting(
            {
                "name": "studio",
                "lights": [
                    {
                        "name": "Key",
                        "type": "AREA",
                        "energy": 800.0,
                        "ies_path": ies_path,
                    },
                ],
                "world": {"strength": 0.1},
            }
        )
        self.assertEqual(out["ies_attached"], ["Key"])
        # The stub records the attachment on the light data.
        key_light = self.stub.data.lights._items["Key"]  # type: ignore[attr-defined]
        self.assertEqual(key_light.ies_profile["path"], ies_path)
        self.assertGreater(key_light.ies_profile["bytes"], 0)

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

    # ----- panorama -----

    def test_panorama_render_configures_pano_camera(self):
        from panorama import render_panorama  # type: ignore

        out = render_panorama(
            "/tmp/pano.png",
            samples=64,
            resolution_x=512,
            resolution_y=256,
            denoise=False,
        )
        self.assertEqual(out["mode"], "panorama")
        self.assertEqual(out["panorama_type"], "EQUIRECTANGULAR")
        self.assertEqual(out["samples"], 64)
        self.assertEqual(out["resolution_x"], 512)
        self.assertEqual(out["resolution_y"], 256)

        import bpy  # type: ignore

        # The camera data block must have type=PANO + panorama_type=EQUIRECTANGULAR.
        cam_data = bpy.data.cameras["PanoramaCamera"]
        self.assertEqual(cam_data["type"], "PANO")
        self.assertEqual(cam_data["panorama_type"], "EQUIRECTANGULAR")

        # Render engine + dimensions must match the panorama preset.
        self.assertEqual(bpy.context.scene.render.engine, "CYCLES")
        self.assertEqual(bpy.context.scene.render.resolution_x, 512)
        self.assertEqual(bpy.context.scene.render.resolution_y, 256)
        # write_still must be true so the image is flushed to disk.
        self.assertTrue(bpy.op_log[-1]["write_still"])

    def test_panorama_render_rejects_non_2to1_aspect(self):
        from panorama import render_panorama  # type: ignore

        with self.assertRaises(ValueError):
            render_panorama(
                "/tmp/pano.png",
                resolution_x=1920,
                resolution_y=1080,
            )

    def test_panorama_render_rejects_unknown_format(self):
        from panorama import render_panorama  # type: ignore

        with self.assertRaises(ValueError):
            render_panorama(
                "/tmp/pano.png",
                resolution_x=512,
                resolution_y=256,
                output_format="WEBP",
            )

    # ----- walkthrough -----

    def test_walkthrough_renders_each_frame(self):
        import tempfile

        from walkthrough import render_walkthrough  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            keyframes = [
                {"frame": 1, "position": [0, -5000, 1500], "target": [0, 0, 1500]},
                {"frame": 10, "position": [3000, -5000, 1500], "target": [0, 0, 1500]},
            ]
            out = render_walkthrough(
                tmp,
                keyframes=keyframes,
                samples=8,
                resolution_x=320,
                resolution_y=180,
                frame_start=1,
                frame_end=3,
            )
            self.assertEqual(out["mode"], "walkthrough")
            self.assertEqual(out["frame_start"], 1)
            self.assertEqual(out["frame_end"], 3)
            self.assertEqual(len(out["frames"]), 3)
            self.assertEqual([f["frame"] for f in out["frames"]], [1, 2, 3])

            import bpy  # type: ignore

            self.assertEqual(bpy.context.scene.render.engine, "CYCLES")
            self.assertEqual(bpy.context.scene.cycles.samples, 8)
            # write_still must be true on every frame render.
            renders = [op for op in bpy.op_log if op.get("op") == "render"]
            self.assertEqual(len(renders), 3)
            self.assertTrue(all(r["write_still"] for r in renders))

    def test_walkthrough_requires_keyframes(self):
        from walkthrough import render_walkthrough  # type: ignore

        with self.assertRaises(ValueError):
            render_walkthrough("/tmp", keyframes=[])

    def test_walkthrough_interpolates_between_keyframes(self):
        # Direct unit test of the interpolation helper rather than going
        # through the bpy stub — this verifies the math without needing
        # the worker side effects.
        from walkthrough import _interp  # type: ignore

        kf = [
            {"frame": 1, "position": [0, 0, 0], "target": [10, 0, 0]},
            {"frame": 11, "position": [10, 0, 0], "target": [10, 10, 0]},
        ]
        pos, target = _interp(kf, 6)
        # halfway between (0,0,0) -> (10,0,0): (5,0,0)
        self.assertAlmostEqual(pos[0], 5.0)
        self.assertAlmostEqual(pos[1], 0.0)
        # halfway between (10,0,0) -> (10,10,0): (10,5,0)
        self.assertAlmostEqual(target[0], 10.0)
        self.assertAlmostEqual(target[1], 5.0)

    def test_walkthrough_rejects_duplicate_keyframes(self):
        from walkthrough import render_walkthrough  # type: ignore

        with self.assertRaises(ValueError):
            render_walkthrough(
                "/tmp",
                keyframes=[
                    {"frame": 1, "position": [0, 0, 0], "target": [1, 0, 0]},
                    {"frame": 1, "position": [0, 0, 0], "target": [1, 0, 0]},
                ],
                frame_start=1,
                frame_end=1,
            )

    def test_walkthrough_rejects_extreme_aspect_ratio(self):
        from walkthrough import render_walkthrough  # type: ignore

        with self.assertRaises(ValueError):
            render_walkthrough(
                "/tmp",
                keyframes=[
                    {"frame": 1, "position": [0, 0, 0], "target": [1, 0, 0]},
                ],
                frame_start=1,
                frame_end=1,
                resolution_x=100,
                resolution_y=10000,
            )

    def test_walkthrough_activates_scene_camera(self):
        # Regression: walkthrough must set scene.camera = cam_object so
        # bpy.ops.render.render() actually uses the walkthrough camera.
        import tempfile

        from walkthrough import render_walkthrough  # type: ignore

        with tempfile.TemporaryDirectory() as tmp:
            render_walkthrough(
                tmp,
                keyframes=[
                    {"frame": 1, "position": [0, -5000, 1500], "target": [0, 0, 1500]},
                    {"frame": 5, "position": [3000, -5000, 1500], "target": [0, 0, 1500]},
                ],
                samples=4,
                resolution_x=320,
                resolution_y=180,
                frame_start=1,
                frame_end=2,
                camera_name="WalkthroughCamera",
            )

            import bpy  # type: ignore

            cam = bpy.context.scene.camera
            self.assertIsNotNone(cam)
            cam_name = (
                cam.get("name") if isinstance(cam, dict) else getattr(cam, "name", None)
            )
            self.assertEqual(cam_name, "WalkthroughCamera")

    def test_walkthrough_look_at_returns_identity_when_target_is_minus_z(self):
        # A Blender camera at identity (Euler 0,0,0) looks down -Z. So a
        # camera at the origin targeting (0,0,-1) should need a zero
        # rotation. The previous implementation returned (pi/2, 0, 0) here,
        # which would point the camera at +Y.
        import math

        from walkthrough import _look_at  # type: ignore

        rot = _look_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0])
        self.assertAlmostEqual(rot[0], 0.0, places=6)
        self.assertAlmostEqual(rot[1], 0.0, places=6)
        self.assertAlmostEqual(rot[2], 0.0, places=6)

        # Targeting (0,1,0) from the origin requires pitching the camera
        # 90° forward so its -Z axis points at +Y.
        rot = _look_at([0.0, 0.0, 0.0], [0.0, 1.0, 0.0])
        self.assertAlmostEqual(rot[0], math.pi / 2.0, places=6)
        self.assertAlmostEqual(rot[1], 0.0, places=6)
        self.assertAlmostEqual(rot[2], 0.0, places=6)

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

    # ----- stitch_frames -----

    def test_stitch_frames_returns_image_sequence_when_ffmpeg_missing(self):
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "frame_00001.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "frame_00002.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            out = stitch_frames(
                str(tmp_path),
                str(tmp_path / "walkthrough.mp4"),
                ffmpeg_path="/definitely/does/not/exist",
            )
            self.assertEqual(out["kind"], "image_sequence")
            self.assertEqual(out["frame_count"], 2)

    def test_stitch_frames_rejects_missing_directory(self):
        from walkthrough import stitch_frames  # type: ignore

        with self.assertRaises(ValueError):
            stitch_frames(
                "/this/path/should/never/exist",
                "/tmp/walkthrough.mp4",
            )

    def test_stitch_frames_rejects_zero_fps(self):
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "frame_00001.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            with self.assertRaises(ValueError):
                stitch_frames(
                    str(tmp_path),
                    str(tmp_path / "out.mp4"),
                    fps=0,
                )

    def test_stitch_frames_rejects_empty_directory(self):
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                stitch_frames(
                    tmp,
                    str(Path(tmp) / "out.mp4"),
                )

    def test_stitch_frames_ignores_non_pattern_images(self):
        # Regression: previously `frame_count` counted every .png/.jpg/.jpeg
        # in the directory, which both bypassed the empty-directory guard
        # when only non-frame images existed and inflated the
        # image-sequence frame_count for FFmpeg-absent callers. Verify
        # that files not matching `frame_pattern` are excluded from the
        # count, and that a directory containing only non-frame images
        # is treated the same as an empty directory.
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "frame_00001.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "frame_00002.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "thumbnail.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "reference.jpg").write_bytes(b"\xff\xd8\xff")
            out = stitch_frames(
                str(tmp_path),
                str(tmp_path / "walkthrough.mp4"),
                ffmpeg_path="/definitely/does/not/exist",
            )
            self.assertEqual(out["kind"], "image_sequence")
            self.assertEqual(
                out["frame_count"],
                2,
                "non-frame images must not be counted",
            )

    def test_stitch_frames_rejects_pattern_without_literal_prefix(self):
        # Regression: a printf pattern like `%05d.png` converts to the
        # glob `*.png`, which would silently match every PNG in the
        # directory (thumbnails, reference frames, anything else) and
        # over-report `frame_count`. Reject the ambiguous pattern at
        # the boundary so callers get a clear error instead of a wrong
        # answer.
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "001.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "thumbnail.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            with self.assertRaises(ValueError) as ctx:
                stitch_frames(
                    str(tmp_path),
                    str(tmp_path / "out.mp4"),
                    frame_pattern="%03d.png",
                )
            self.assertIn("literal prefix", str(ctx.exception))

        # And a pattern with no `%d` at all should also be rejected, so
        # callers passing in a static filename by mistake fail fast.
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError) as ctx:
                stitch_frames(
                    tmp,
                    str(Path(tmp) / "out.mp4"),
                    frame_pattern="frame.png",
                )
            self.assertIn("printf placeholder", str(ctx.exception))

    def test_stitch_frames_rejects_directory_with_only_non_pattern_images(self):
        from walkthrough import stitch_frames  # type: ignore
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "thumbnail.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "reference.jpg").write_bytes(b"\xff\xd8\xff")
            with self.assertRaises(ValueError) as ctx:
                stitch_frames(
                    str(tmp_path),
                    str(tmp_path / "out.mp4"),
                )
            self.assertIn("frame_%05d.png", str(ctx.exception))

    def test_stitch_frames_invokes_ffmpeg_when_present(self):
        from walkthrough import stitch_frames  # type: ignore
        import tempfile
        import textwrap
        import os
        import stat

        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "frame_00001.png").write_bytes(b"\x89PNG\r\n\x1a\n")
            (tmp_path / "frame_00002.png").write_bytes(b"\x89PNG\r\n\x1a\n")

            fake_ffmpeg = tmp_path / "fake_ffmpeg"
            fake_ffmpeg.write_text(textwrap.dedent("""\
                #!/usr/bin/env bash
                touch \"${@: -1}\"
                exit 0
                """))
            os.chmod(fake_ffmpeg, os.stat(fake_ffmpeg).st_mode | stat.S_IEXEC)

            output = tmp_path / "out.mp4"
            result = stitch_frames(
                str(tmp_path),
                str(output),
                fps=24,
                ffmpeg_path=str(fake_ffmpeg),
            )
            self.assertEqual(result["kind"], "video")
            self.assertTrue(output.exists())
            self.assertEqual(result["fps"], 24)


if __name__ == "__main__":
    unittest.main()
