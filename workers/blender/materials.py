"""Translate AEC PBR material definitions into Blender material data.

The Rust side ships a flat material spec (see `aec_materials::PbrMaterial`)
serialised as JSON. This module materialises each spec into Blender's
`bpy.data.materials` (or the test stub) by writing the Principled-BSDF
inputs the renderer engines understand.
"""

from __future__ import annotations

from typing import Any


def _bpy() -> Any:
    import bpy

    return bpy


def apply_materials(materials: list[dict[str, Any]]) -> dict[str, Any]:
    """Create or update every material in the supplied list. Returns a
    summary suitable for the IPC response.
    """
    bpy = _bpy()
    created: list[str] = []
    updated: list[str] = []
    for mat in materials:
        name = mat["name"]
        if name in bpy.data.materials:
            m = bpy.data.materials[name]
            updated.append(name)
        else:
            m = bpy.data.materials.new(name)
            created.append(name)
        m.base_color = _color4(mat.get("albedo", [0.8, 0.8, 0.8]))
        m.metallic = float(mat.get("metallic", 0.0))
        m.roughness = float(mat.get("roughness", 0.5))
        m.normal_strength = float(mat.get("normal_strength", 1.0))
        m.use_nodes = True
    return {
        "created": created,
        "updated": updated,
        "total": len(created) + len(updated),
    }


def _color4(value) -> tuple[float, float, float, float]:
    if isinstance(value, (list, tuple)):
        if len(value) == 3:
            return (float(value[0]), float(value[1]), float(value[2]), 1.0)
        if len(value) == 4:
            return (float(value[0]), float(value[1]), float(value[2]), float(value[3]))
    return (0.8, 0.8, 0.8, 1.0)
