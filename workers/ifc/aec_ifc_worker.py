"""AEC Studio IFC worker entrypoint.

Same JSON-line stdio protocol as the Blender worker. Methods:

* `ifc.import` — params: `{"path": str}` → returns the AEC graph dict.
* `ifc.export` — params: `{"graph": {...}, "path": str}` → returns a
  summary with the project GUID and element count.
* `ifc.validate` — params: `{"graph": {...}}` → returns
  `{"ok": bool, "errors": [...], "warnings": [...]}`.
* `ping` / `shutdown`.
"""

from __future__ import annotations

import json
import sys
import traceback
from pathlib import Path
from typing import Any, Callable, IO

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from export_pipeline import export_ifc  # noqa: E402
from import_pipeline import import_ifc  # noqa: E402
from validator import validate  # noqa: E402


Method = Callable[[dict[str, Any]], Any]


def _methods() -> dict[str, Method]:
    return {
        "ifc.import": lambda p: import_ifc(p["path"]),
        "ifc.export": lambda p: export_ifc(p["graph"], p["path"]),
        "ifc.validate": lambda p: validate(p["graph"]),
        "ping": lambda _p: {"pong": True},
    }


def dispatch(request: dict[str, Any], methods: dict[str, Method] | None = None) -> dict[str, Any]:
    methods = methods or _methods()
    rid = request.get("id")
    method = request.get("method")
    params = request.get("params", {}) or {}
    if not isinstance(method, str):
        return {"id": rid, "error": {"code": "BAD_REQUEST", "message": "missing 'method'"}}
    if method not in methods:
        return {"id": rid, "error": {"code": "UNKNOWN_METHOD", "message": method}}
    try:
        return {"id": rid, "result": methods[method](params)}
    except Exception as exc:  # noqa: BLE001 — worker boundary
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
