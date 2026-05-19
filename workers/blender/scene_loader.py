"""Load an AEC scene description into Blender objects.

The Rust side serialises the scene graph to a small JSON document:

```json
{
  "scene_name": "Default",
  "units": "mm",
  "meshes": [
    {"name": "Wall_001", "vertices": [[0,0,0], [4500,0,0], ...],
     "faces": [[0,1,2,3], ...], "material": "wall_white"}
  ],
  "lights": [{"name": "Sun", "type": "SUN", "energy": 4.5}],
  "camera": {"name": "Cam", "location": [0,0,1700], "rotation": [0,0,0],
             "focal_length_mm": 35}
}
```

This module deserialises that document into Blender objects (or the stub
when running in tests).
"""

from __future__ import annotations

import math
from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def load_scene(scene: dict[str, Any]) -> dict[str, Any]:
    """Materialise the scene description and return a summary dictionary."""
    bpy = _bpy()
    name = scene.get("scene_name", "Scene")
    # Default scene is already named "Scene" — reuse it; otherwise add a new
    # scene to bpy.data.scenes. Both real Blender's BlendDataScenes and the
    # in-process stub support the `in` operator.
    if name in bpy.data.scenes:
        s = bpy.data.scenes[name]
    else:
        s = bpy.data.scenes.new(name)

    created_meshes: list[str] = []
    created_lights: list[str] = []

    # Unit scale: AEC works in mm; Blender prefers metres. We scale on load.
    unit_scale = _unit_scale(scene.get("units", "mm"))

    for mesh_def in scene.get("meshes", []):
        mesh_name = mesh_def["name"]
        m = bpy.data.meshes.new(mesh_name)
        verts = [_scale_vec(v, unit_scale) for v in mesh_def.get("vertices", [])]
        faces = [tuple(f) for f in mesh_def.get("faces", [])]
        m.from_pydata(verts, [], faces)
        m.update()
        obj = bpy.data.objects.new(mesh_name, m)
        s.objects.link(obj)
        if "material" in mesh_def and mesh_def["material"]:
            obj.material_slots.append(mesh_def["material"])
        created_meshes.append(mesh_name)

    for light_def in scene.get("lights", []):
        lname = light_def["name"]
        light_data = bpy.data.lights.new(lname, type=light_def.get("type", "SUN"))
        light_data.energy = float(light_def.get("energy", 1000.0))
        if "color" in light_def:
            light_data.color = tuple(light_def["color"])
        obj = bpy.data.objects.new(lname, light_data)
        if "location" in light_def:
            loc = _scale_vec(light_def["location"], unit_scale)
            obj.location.x, obj.location.y, obj.location.z = loc
        s.objects.link(obj)
        created_lights.append(lname)

    camera = scene.get("camera")
    if camera:
        cam_record = {
            "location": _scale_vec(camera.get("location", [0, 0, 0]), unit_scale),
            "rotation": camera.get("rotation", [0, 0, 0]),
            "focal_length_mm": float(camera.get("focal_length_mm", 35.0)),
            "horizontal_fov_radians": _hfov_from_focal(
                float(camera.get("focal_length_mm", 35.0)),
                float(camera.get("sensor_width_mm", 36.0)),
            ),
        }
        bpy.data.cameras[camera.get("name", "Camera")] = cam_record

    return {
        "scene_name": name,
        "mesh_count": len(created_meshes),
        "light_count": len(created_lights),
        "meshes": created_meshes,
        "lights": created_lights,
    }


def _unit_scale(units: str) -> float:
    if units == "mm":
        return 0.001
    if units in ("m", "meters"):
        return 1.0
    if units in ("inches", "in"):
        return 0.0254
    if units in ("ft", "feet"):
        return 0.3048
    return 1.0


def _scale_vec(vec, scale: float):
    return tuple(float(c) * scale for c in vec)


def _hfov_from_focal(focal_mm: float, sensor_mm: float) -> float:
    if focal_mm <= 0.0:
        return math.pi / 2
    return 2.0 * math.atan2(sensor_mm * 0.5, focal_mm)
