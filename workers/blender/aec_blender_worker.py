"""AEC Studio Blender worker.

Run as a child process of `aec_bridge`. Communicates via stdin/stdout
using JSON-line framing.

Request envelope:
```json
{"id": 17, "method": "render.eevee_preview", "params": {...}}
```

Response envelope:
```json
{"id": 17, "result": {...}}  // success
{"id": 17, "error": {"code": "...", "message": "..."}}  // failure
```

Methods:
* `scene.load` — params: full AEC scene JSON.
* `render.eevee_preview` — params: `{"output": str, "resolution_x": int, "resolution_y": int}`.
* `render.cycles_final` — params: `{"output": str, "samples": int, ...}`.
* `materials.apply` — params: `{"materials": [...]}`.
* `lighting.apply` — params: a lighting preset dict.
* `ping` — params ignored; returns `{"pong": true}`.
* `shutdown` — exits the worker cleanly.
"""

from __future__ import annotations

import json
import sys
import traceback
from typing import Any, Callable, IO

from cycles_final import render_cycles_final
from eevee_preview import render_eevee_preview
from lighting import apply_lighting
from materials import apply_materials
from panorama import render_panorama
from scene_loader import load_scene
from walkthrough import render_walkthrough


Method = Callable[[dict[str, Any]], Any]


def _methods() -> dict[str, Method]:
    return {
        "scene.load": load_scene,
        "render.eevee_preview": lambda p: render_eevee_preview(
            out_path=p["output"],
            resolution_x=int(p.get("resolution_x", 1280)),
            resolution_y=int(p.get("resolution_y", 720)),
        ),
        "render.cycles_final": lambda p: render_cycles_final(
            out_path=p["output"],
            samples=int(p.get("samples", 128)),
            resolution_x=int(p.get("resolution_x", 1920)),
            resolution_y=int(p.get("resolution_y", 1080)),
            denoise=bool(p.get("denoise", True)),
            device=str(p.get("device", "CPU")),
            tile_size=int(p.get("tile_size", 256)),
        ),
        "render.panorama": lambda p: render_panorama(
            out_path=p["output"],
            samples=int(p.get("samples", 512)),
            resolution_x=int(p.get("resolution_x", 4096)),
            resolution_y=int(p.get("resolution_y", 2048)),
            denoise=bool(p.get("denoise", True)),
            device=str(p.get("device", "CPU")),
            output_format=str(p.get("output_format", "PNG")),
            camera_name=str(p.get("camera_name", "PanoramaCamera")),
        ),
        "render.walkthrough": lambda p: render_walkthrough(
            out_dir=p["output_dir"],
            keyframes=p.get("keyframes", []),
            samples=int(p.get("samples", 96)),
            resolution_x=int(p.get("resolution_x", 1920)),
            resolution_y=int(p.get("resolution_y", 1080)),
            denoise=bool(p.get("denoise", True)),
            device=str(p.get("device", "CPU")),
            frame_start=int(p.get("frame_start", 1)),
            frame_end=int(p.get("frame_end", 60)),
        ),
        "materials.apply": lambda p: apply_materials(p.get("materials", [])),
        "lighting.apply": apply_lighting,
        "ping": lambda _p: {"pong": True},
    }


def dispatch(request: dict[str, Any], methods: dict[str, Method] | None = None) -> dict[str, Any]:
    """Dispatch a single request envelope; return the response envelope."""
    methods = methods or _methods()
    rid = request.get("id")
    method = request.get("method")
    params = request.get("params", {}) or {}
    if not isinstance(method, str):
        return {"id": rid, "error": {"code": "BAD_REQUEST", "message": "missing 'method'"}}
    if method not in methods:
        return {"id": rid, "error": {"code": "UNKNOWN_METHOD", "message": method}}
    try:
        result = methods[method](params)
        return {"id": rid, "result": result}
    except Exception as exc:  # noqa: BLE001 — worker boundary: surface to caller
        return {
            "id": rid,
            "error": {
                "code": "EXCEPTION",
                "message": str(exc),
                "type": type(exc).__name__,
                "traceback": traceback.format_exc(),
            },
        }


def serve(
    stdin: IO[str] | None = None,
    stdout: IO[str] | None = None,
    methods: dict[str, Method] | None = None,
) -> int:
    """Read JSON-line requests from stdin and write responses to stdout.

    Returns the exit code (0 on clean shutdown). Designed to be testable
    without spawning a subprocess: callers pass `io.StringIO` instances.
    """
    sin = stdin or sys.stdin
    sout = stdout or sys.stdout
    methods = methods or _methods()
    for raw in sin:
        line = raw.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as e:
            sout.write(
                json.dumps({"id": None, "error": {"code": "BAD_JSON", "message": str(e)}}) + "\n"
            )
            sout.flush()
            continue
        if isinstance(request, dict) and request.get("method") == "shutdown":
            sout.write(json.dumps({"id": request.get("id"), "result": {"shutdown": True}}) + "\n")
            sout.flush()
            return 0
        response = dispatch(request, methods)
        sout.write(json.dumps(response) + "\n")
        sout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover — entrypoint
    raise SystemExit(serve())
