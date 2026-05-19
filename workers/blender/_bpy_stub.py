"""In-process stub of the parts of `bpy` (Blender's Python API) that the
worker touches. Used for unit tests and any environment without Blender.

The stub records calls and stores synthesised objects so tests can
inspect what the worker would have done inside a real Blender process.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable


@dataclass
class _Vec3:
    x: float = 0.0
    y: float = 0.0
    z: float = 0.0

    def __iter__(self):
        yield self.x
        yield self.y
        yield self.z


@dataclass
class _Mesh:
    name: str
    vertices: list[tuple[float, float, float]] = field(default_factory=list)
    faces: list[tuple[int, ...]] = field(default_factory=list)
    uvs: list[tuple[float, float]] = field(default_factory=list)
    materials: list[str] = field(default_factory=list)

    def from_pydata(self, verts, edges, faces):
        self.vertices = [tuple(v) for v in verts]
        self.faces = [tuple(f) for f in faces]

    def update(self):
        pass


@dataclass
class _Object:
    name: str
    type: str
    data: Any = None
    location: _Vec3 = field(default_factory=_Vec3)
    rotation_euler: _Vec3 = field(default_factory=_Vec3)
    scale: _Vec3 = field(default_factory=lambda: _Vec3(1.0, 1.0, 1.0))
    parent: "_Object | None" = None
    material_slots: list[str] = field(default_factory=list)


@dataclass
class _Material:
    name: str
    base_color: tuple[float, float, float, float] = (0.8, 0.8, 0.8, 1.0)
    metallic: float = 0.0
    roughness: float = 0.5
    normal_strength: float = 1.0
    use_nodes: bool = True


@dataclass
class _Light:
    name: str
    type: str
    energy: float = 1000.0
    color: tuple[float, float, float] = (1.0, 1.0, 1.0)


class _MeshesCollection:
    def __init__(self) -> None:
        self._items: dict[str, _Mesh] = {}

    def new(self, name: str) -> _Mesh:
        m = _Mesh(name=name)
        self._items[name] = m
        return m

    def __iter__(self):
        return iter(self._items.values())


class _ObjectsCollection:
    def __init__(self) -> None:
        self._items: dict[str, _Object] = {}

    def new(self, name: str, data: Any) -> _Object:
        obj_type = "MESH" if isinstance(data, _Mesh) else "LIGHT"
        o = _Object(name=name, type=obj_type, data=data)
        self._items[name] = o
        return o

    def link(self, obj: _Object) -> None:
        self._items[obj.name] = obj

    def __iter__(self):
        return iter(self._items.values())

    def __len__(self) -> int:
        return len(self._items)


class _MaterialsCollection:
    def __init__(self) -> None:
        self._items: dict[str, _Material] = {}

    def new(self, name: str) -> _Material:
        m = _Material(name=name)
        self._items[name] = m
        return m

    def __contains__(self, key: str) -> bool:
        return key in self._items

    def __getitem__(self, key: str) -> _Material:
        return self._items[key]


class _LightsCollection:
    def __init__(self) -> None:
        self._items: dict[str, _Light] = {}

    def new(self, name: str, type: str) -> _Light:
        light = _Light(name=name, type=type)
        self._items[name] = light
        return light


class _Scene:
    def __init__(self, name: str) -> None:
        self.name = name
        self.render = _Render()
        self.cycles = _Cycles()
        self.objects = _ObjectsCollection()
        self.frame_current = 1


class _Render:
    engine: str = "BLENDER_EEVEE"
    resolution_x: int = 1920
    resolution_y: int = 1080
    resolution_percentage: int = 100
    filepath: str = "/tmp/render.png"
    image_settings_file_format: str = "PNG"


class _Cycles:
    samples: int = 128
    use_denoising: bool = True
    denoiser: str = "OPENIMAGEDENOISE"
    device: str = "CPU"
    tile_size: int = 256


class _ScenesCollection:
    def __init__(self) -> None:
        self._items: dict[str, _Scene] = {"Scene": _Scene("Scene")}

    def new(self, name: str) -> _Scene:
        s = _Scene(name)
        self._items[name] = s
        return s

    def __getitem__(self, key: str) -> _Scene:
        return self._items[key]

    def __contains__(self, key: object) -> bool:
        return isinstance(key, str) and key in self._items

    def __iter__(self):
        return iter(self._items.values())

    def __len__(self) -> int:
        return len(self._items)


class _Data:
    def __init__(self) -> None:
        self.meshes = _MeshesCollection()
        self.objects = _ObjectsCollection()
        self.materials = _MaterialsCollection()
        self.lights = _LightsCollection()
        self.scenes = _ScenesCollection()
        self.cameras: dict[str, dict[str, Any]] = {}


class _Context:
    def __init__(self, data: _Data) -> None:
        self.scene = data.scenes["Scene"]
        self.view_layer = type("ViewLayer", (), {"objects": {"active": None}})()


class _OpsRender:
    """Records `render()` invocations rather than actually rendering."""

    def __init__(self, log: list[dict[str, Any]]) -> None:
        self._log = log

    def render(self, write_still: bool = False, animation: bool = False) -> None:
        self._log.append(
            {"op": "render", "write_still": write_still, "animation": animation}
        )


class _Ops:
    def __init__(self, log: list[dict[str, Any]]) -> None:
        self.render = _OpsRender(log)


class BpyStub:
    """Top-level stub exposing the `bpy.data`, `bpy.context`, `bpy.ops`
    namespaces. Construct one per scene/test so state never leaks.
    """

    def __init__(self) -> None:
        self.data = _Data()
        self.context = _Context(self.data)
        self._op_log: list[dict[str, Any]] = []
        self.ops = _Ops(self._op_log)

    @property
    def op_log(self) -> list[dict[str, Any]]:
        return list(self._op_log)


def install_as_bpy(monkeypatch_or_globals: dict[str, Any] | None = None) -> BpyStub:
    """Make a stub available under the name `bpy` in `sys.modules`. Returns
    the stub instance so tests can inspect it. Use [`uninstall`] to clean
    up.
    """
    import sys

    stub = BpyStub()
    sys.modules["bpy"] = stub  # type: ignore[assignment]
    return stub


def uninstall() -> None:
    import sys

    if "bpy" in sys.modules:
        del sys.modules["bpy"]


def call_with_stub(fn: Callable[..., Any], *args: Any, **kwargs: Any) -> tuple[Any, BpyStub]:
    """Convenience: install stub, call `fn`, return its result and the
    stub. Always uninstalls on exit.
    """
    stub = install_as_bpy()
    try:
        result = fn(*args, **kwargs)
    finally:
        uninstall()
    return result, stub
