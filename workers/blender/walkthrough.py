"""Cycles walkthrough (camera-path animation) render.

Renders an image sequence by animating a single camera between
keyframes. The Rust side produces a `CameraPath` with absolute
keyframes (position + target + frame); we translate that to a Blender
animation curve, then render `[frame_start, frame_end]` as PNGs into
`out_dir`.

Progress is reported per frame so the Rust queue can resume from
`last_completed_frame + 1` if the job fails partway through.
"""

from __future__ import annotations

import math
from pathlib import Path
from typing import Any, Iterable


def _bpy() -> Any:
    import bpy

    return bpy


def render_walkthrough(
    out_dir: str,
    keyframes: list[dict[str, Any]],
    *,
    samples: int = 96,
    resolution_x: int = 1920,
    resolution_y: int = 1080,
    denoise: bool = True,
    device: str = "CPU",
    frame_start: int = 1,
    frame_end: int = 60,
    camera_name: str = "WalkthroughCamera",
    progress_sink: Any = None,
) -> dict[str, Any]:
    """Render the walkthrough as an image sequence under `out_dir`.

    `keyframes` must be a non-empty list of dicts shaped:
        {"frame": int, "position": [x,y,z], "target": [x,y,z]}

    The function returns the list of rendered frames so the Rust queue
    can mark them as complete; if the caller is resuming a failed job,
    they pass the new `frame_start` (= last_completed_frame + 1).
    """

    if not keyframes:
        raise ValueError("walkthrough requires at least one keyframe")
    if frame_end < frame_start:
        raise ValueError(
            f"frame_end ({frame_end}) must be >= frame_start ({frame_start})"
        )
    _validate_keyframes(keyframes)

    out_path = Path(out_dir)
    out_path.mkdir(parents=True, exist_ok=True)

    bpy = _bpy()
    scene = bpy.context.scene

    scene.render.engine = "CYCLES"
    scene.render.resolution_x = int(resolution_x)
    scene.render.resolution_y = int(resolution_y)
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.cycles.samples = int(samples)
    scene.cycles.use_denoising = bool(denoise)
    scene.cycles.denoiser = "OPENIMAGEDENOISE"
    scene.cycles.device = device.upper()

    cam_data = _ensure_camera_data(bpy, camera_name)
    cam_object = _ensure_camera_object(bpy, cam_data, camera_name)

    # Sort keyframes by frame so interpolation is monotonic.
    sorted_kf = sorted(keyframes, key=lambda k: int(k["frame"]))

    rendered: list[dict[str, Any]] = []
    for frame in range(int(frame_start), int(frame_end) + 1):
        position, target = _interp(sorted_kf, frame)
        _set_camera_transform(cam_object, position, target)
        scene.frame_current = frame
        frame_path = out_path / f"frame_{frame:05d}.png"
        scene.render.filepath = str(frame_path)
        bpy.ops.render.render(write_still=True)
        rendered.append(
            {
                "frame": frame,
                "path": str(frame_path.absolute()),
                "position": list(position),
                "target": list(target),
            }
        )
        if progress_sink is not None:
            try:
                progress_sink(frame, frame_end)
            except Exception:  # pragma: no cover - never fail the render on progress write
                pass

    return {
        "engine": "cycles",
        "mode": "walkthrough",
        "out_dir": str(out_path.absolute()),
        "samples": int(samples),
        "resolution_x": int(resolution_x),
        "resolution_y": int(resolution_y),
        "denoise": bool(denoise),
        "device": device.upper(),
        "camera": camera_name,
        "frame_start": int(frame_start),
        "frame_end": int(frame_end),
        "frames": rendered,
    }


def _validate_keyframes(keyframes: Iterable[dict[str, Any]]) -> None:
    seen: set[int] = set()
    for kf in keyframes:
        if "frame" not in kf or "position" not in kf or "target" not in kf:
            raise ValueError(
                "every keyframe must have frame/position/target keys"
            )
        f = int(kf["frame"])
        if f in seen:
            raise ValueError(f"duplicate keyframe at frame {f}")
        seen.add(f)
        if len(kf["position"]) != 3 or len(kf["target"]) != 3:
            raise ValueError("position and target must be 3-element vectors")


def _interp(sorted_kf: list[dict[str, Any]], frame: int) -> tuple[list[float], list[float]]:
    """Linearly interpolate (position, target) at `frame`. Clamps to
    the first/last keyframe outside the range.
    """

    if frame <= int(sorted_kf[0]["frame"]):
        first = sorted_kf[0]
        return list(first["position"]), list(first["target"])
    if frame >= int(sorted_kf[-1]["frame"]):
        last = sorted_kf[-1]
        return list(last["position"]), list(last["target"])

    # Find bracket [a, b] containing frame.
    for i in range(len(sorted_kf) - 1):
        a = sorted_kf[i]
        b = sorted_kf[i + 1]
        fa = int(a["frame"])
        fb = int(b["frame"])
        if fa <= frame <= fb:
            if fb == fa:
                return list(a["position"]), list(a["target"])
            t = (frame - fa) / (fb - fa)
            return (
                _lerp_vec3(a["position"], b["position"], t),
                _lerp_vec3(a["target"], b["target"], t),
            )
    # Shouldn't reach here because of the clamp above, but be defensive.
    last = sorted_kf[-1]
    return list(last["position"]), list(last["target"])


def _lerp_vec3(a: list[float], b: list[float], t: float) -> list[float]:
    return [float(a[i]) + (float(b[i]) - float(a[i])) * t for i in range(3)]


def _ensure_camera_data(bpy: Any, name: str) -> Any:
    cameras = bpy.data.cameras
    if hasattr(cameras, "new"):
        cam = cameras.new(name=name)
    elif isinstance(cameras, dict):
        cam = cameras.get(name)
        if cam is None:
            cam = {"name": name}
            cameras[name] = cam
    else:  # pragma: no cover
        raise RuntimeError("bpy.data.cameras has unexpected shape")
    _set_attr(cam, "type", "PERSP")
    return cam


def _ensure_camera_object(bpy: Any, cam_data: Any, name: str) -> Any:
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
    obj_type = getattr(obj, "type", None) if not isinstance(obj, dict) else obj.get("type")
    if obj_type not in ("CAMERA",):
        _set_attr(obj, "type", "CAMERA")
    return obj


def _set_camera_transform(obj: Any, position: list[float], target: list[float]) -> None:
    # Real Blender sets location + rotation_euler from a look-at quaternion;
    # the stub stores them as _Vec3 / dict entries we can introspect.
    _set_vec3(obj, "location", position)
    rot = _look_at(position, target)
    _set_vec3(obj, "rotation_euler", rot)


def _set_vec3(obj: Any, attr: str, vec: list[float]) -> None:
    # If the target is a dict, store as a tuple; otherwise mutate the
    # stub _Vec3 in place (real Blender accepts both tuple assignments
    # and per-axis updates).
    if isinstance(obj, dict):
        obj[attr] = tuple(float(c) for c in vec)
    else:
        target = getattr(obj, attr, None)
        if target is None or not hasattr(target, "x"):
            setattr(obj, attr, tuple(float(c) for c in vec))
            return
        target.x = float(vec[0])
        target.y = float(vec[1])
        target.z = float(vec[2])


def _look_at(position: list[float], target: list[float]) -> list[float]:
    """Euler XYZ that orients a camera at `position` to look at `target`.

    This is the standard Blender camera convention: looking down -Z
    locally, with +Y up. The math here is straightforward — derive
    the forward vector, then convert to yaw (Z) and pitch (X).
    """

    fx, fy, fz = (
        float(target[0]) - float(position[0]),
        float(target[1]) - float(position[1]),
        float(target[2]) - float(position[2]),
    )
    length = math.sqrt(fx * fx + fy * fy + fz * fz)
    if length == 0:
        return [0.0, 0.0, 0.0]
    # Blender default camera looks down -Z. Compute Euler XYZ.
    pitch = math.atan2(-fz, math.sqrt(fx * fx + fy * fy))
    yaw = math.atan2(fx, fy)
    return [pitch, 0.0, yaw]


def _set_attr(target: Any, name: str, value: Any) -> None:
    if isinstance(target, dict):
        target[name] = value
    else:
        setattr(target, name, value)
