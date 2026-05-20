"""EEVEE preview render path.

The Rust render queue submits a Preview job whose preset is `eevee`. This
module sets EEVEE-specific scene settings and triggers a single-frame
render to disk. The function returns the absolute path of the produced
PNG so the bridge can stream it to the renderer.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def render_eevee_preview(out_path: str, *, resolution_x: int = 1280, resolution_y: int = 720) -> dict[str, Any]:
    bpy = _bpy()
    scene = bpy.context.scene
    scene.render.engine = "BLENDER_EEVEE"
    scene.render.resolution_x = int(resolution_x)
    scene.render.resolution_y = int(resolution_y)
    scene.render.resolution_percentage = 100
    scene.render.filepath = str(out_path)
    scene.render.image_settings.file_format = "PNG"
    bpy.ops.render.render(write_still=True)
    return {
        "engine": "eevee",
        "output": str(Path(out_path).absolute()),
        "resolution_x": int(resolution_x),
        "resolution_y": int(resolution_y),
    }
