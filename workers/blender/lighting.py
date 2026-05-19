"""Configure scene-level lighting from an AEC lighting preset.

Each preset is a dict like:

```json
{
  "name": "warm_evening",
  "lights": [
    {"name": "Sun", "type": "SUN", "energy": 0.6, "color": [1.0, 0.6, 0.3]},
    {"name": "Fill", "type": "AREA", "energy": 200, "color": [1.0, 0.9, 0.85]}
  ],
  "world": {"strength": 0.15, "color": [0.05, 0.05, 0.07]}
}
```
"""

from __future__ import annotations

from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def apply_lighting(preset: dict[str, Any]) -> dict[str, Any]:
    bpy = _bpy()
    lights = preset.get("lights", [])
    created_lights: list[str] = []
    for spec in lights:
        name = spec["name"]
        light_data = bpy.data.lights.new(name, type=spec.get("type", "SUN"))
        light_data.energy = float(spec.get("energy", 1000.0))
        if "color" in spec:
            light_data.color = tuple(spec["color"])
        obj = bpy.data.objects.new(name, light_data)
        scene = bpy.context.scene
        scene.objects.link(obj)
        created_lights.append(name)
    world = preset.get("world", {})
    return {
        "preset": preset.get("name", "unnamed"),
        "lights": created_lights,
        "world_strength": float(world.get("strength", 1.0)),
    }
