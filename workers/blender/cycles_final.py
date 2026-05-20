"""Cycles final render path with OIDN denoising."""

from __future__ import annotations

from pathlib import Path
from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def render_cycles_final(
    out_path: str,
    *,
    samples: int = 128,
    resolution_x: int = 1920,
    resolution_y: int = 1080,
    denoise: bool = True,
    device: str = "CPU",
    tile_size: int = 256,
) -> dict[str, Any]:
    bpy = _bpy()
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.render.resolution_x = int(resolution_x)
    scene.render.resolution_y = int(resolution_y)
    scene.render.resolution_percentage = 100
    scene.render.filepath = str(out_path)
    scene.render.image_settings.file_format = "PNG"
    scene.cycles.samples = int(samples)
    scene.cycles.use_denoising = bool(denoise)
    scene.cycles.denoiser = "OPENIMAGEDENOISE"
    scene.cycles.device = device.upper()
    scene.cycles.tile_size = int(tile_size)
    bpy.ops.render.render(write_still=True)
    return {
        "engine": "cycles",
        "output": str(Path(out_path).absolute()),
        "samples": int(samples),
        "resolution_x": int(resolution_x),
        "resolution_y": int(resolution_y),
        "denoise": bool(denoise),
        "device": device.upper(),
    }
