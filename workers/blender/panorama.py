"""Cycles equirectangular 360° panorama render.

Mirrors `cycles_final.py` but switches the active camera to a `PANO`
camera with `EQUIRECTANGULAR` panorama type at 4096×2048. Output is a
single equirectangular image suitable for HDRI viewers, virtual tours,
or further compositing.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def render_panorama(
    out_path: str,
    *,
    samples: int = 512,
    resolution_x: int = 4096,
    resolution_y: int = 2048,
    denoise: bool = True,
    device: str = "CPU",
    output_format: str = "PNG",
    camera_name: str = "PanoramaCamera",
) -> dict[str, Any]:
    """Configure and trigger an equirectangular panorama render.

    Returns the metadata bag that the worker reports back to Rust via
    JSON-line IPC.

    The aspect ratio must be exactly 2:1 — equirectangular images map
    longitude (0..2π) across X and latitude (-π/2..π/2) across Y. We
    enforce this so silently-wrong outputs don't reach the user.
    """

    if resolution_x != 2 * resolution_y:
        raise ValueError(
            "panorama resolution must be 2:1 "
            f"(got {resolution_x}x{resolution_y})"
        )
    if output_format.upper() not in {"PNG", "JPEG", "OPEN_EXR"}:
        raise ValueError(
            f"unsupported panorama output_format {output_format!r}; "
            "expected PNG, JPEG, or OPEN_EXR"
        )

    bpy = _bpy()
    scene = bpy.context.scene

    # 1. Configure render output.
    scene.render.engine = "CYCLES"
    scene.render.resolution_x = int(resolution_x)
    scene.render.resolution_y = int(resolution_y)
    scene.render.resolution_percentage = 100
    scene.render.filepath = str(out_path)
    scene.render.image_settings.file_format = output_format.upper()

    # 2. Cycles config — panorama renders need a lot of samples to avoid
    # fireflies because every direction contributes.
    scene.cycles.samples = int(samples)
    scene.cycles.use_denoising = bool(denoise)
    scene.cycles.denoiser = "OPENIMAGEDENOISE"
    scene.cycles.device = device.upper()

    # 3. Create / fetch the panorama camera and switch the scene to it.
    cam_data = _ensure_panorama_camera_data(bpy, camera_name)
    cam_object = _ensure_panorama_camera_object(bpy, cam_data, camera_name)
    scene.camera = cam_object

    # 4. Render.
    bpy.ops.render.render(write_still=True)

    return {
        "engine": "cycles",
        "mode": "panorama",
        "panorama_type": "EQUIRECTANGULAR",
        "output": str(Path(out_path).absolute()),
        "output_format": output_format.upper(),
        "samples": int(samples),
        "resolution_x": int(resolution_x),
        "resolution_y": int(resolution_y),
        "denoise": bool(denoise),
        "device": device.upper(),
        "camera": camera_name,
    }


def _ensure_panorama_camera_data(bpy: Any, name: str) -> Any:
    """Create or fetch a camera data block configured as a Cycles
    equirectangular panorama camera.
    """

    cameras = bpy.data.cameras
    # The real bpy.data.cameras is collection-like; our stub uses a dict.
    if hasattr(cameras, "new"):
        cam_data = cameras.new(name=name)
    elif isinstance(cameras, dict):
        cam_data = cameras.get(name)
        if cam_data is None:
            cam_data = {"name": name}
            cameras[name] = cam_data
    else:  # pragma: no cover - extremely defensive
        raise RuntimeError("bpy.data.cameras has unexpected shape")

    _set_attr(cam_data, "type", "PANO")
    _set_attr(cam_data, "panorama_type", "EQUIRECTANGULAR")
    return cam_data


def _ensure_panorama_camera_object(bpy: Any, cam_data: Any, name: str) -> Any:
    """Create / fetch the panorama camera object. Real Blender's
    `bpy.data.objects.new(name, data)` takes name and data — the data
    object's class drives the resulting `Object.type`.
    """

    objects = bpy.data.objects
    if hasattr(objects, "new") and not isinstance(objects, dict):
        obj = objects.new(name=name, data=cam_data)
    elif isinstance(objects, dict):
        obj = objects.get(name)
        if obj is None:
            obj = {"name": name, "type": "CAMERA", "data": cam_data}
            objects[name] = obj
    else:  # pragma: no cover
        raise RuntimeError("bpy.data.objects has unexpected shape")

    _set_attr(obj, "data", cam_data)
    # Ensure the object is marked as a camera even if the stub's
    # type-inference (mesh / light heuristic) couldn't classify it.
    obj_type = getattr(obj, "type", None) if not isinstance(obj, dict) else obj.get("type")
    if obj_type not in ("CAMERA",):
        _set_attr(obj, "type", "CAMERA")
    return obj


def _set_attr(target: Any, name: str, value: Any) -> None:
    """Set attribute name on `target`, supporting both real Blender
    objects and the dict-shaped stub fixtures."""
    if isinstance(target, dict):
        target[name] = value
    else:
        setattr(target, name, value)
