"""Generic OrcaRein hardware sidecar engine.

Reads one JSON request per stdin line, replies with one JSON line per request.
Maintains a persistent Python namespace ``ns`` across all calls, so driver
objects (e.g. ServoKit handles) created in an ``init`` block survive for the
lifetime of the process.

Protocol
--------
Request  (one JSON object per line)::

    {"id": <int>, "kind": "init"|"exec"|"eval", "code": "<python>"}

``init`` / ``exec``
    ``exec(code, ns)``; result field is ``null``.

``eval``
    ``eval(code, ns)``; result field is the return value (must be
    JSON-serialisable).

Unknown ``kind`` → error response.

Response (one JSON object per line)::

    {"id": <int>, "ok": true,  "result": <value-or-null>, "error": null}
    {"id": <int>, "ok": false, "result": null,            "error": "<repr(exc)>"}
"""

import sys
import json

ns = {}


def handle(req):
    """Dispatch a single parsed request dict; return the result value or raise."""
    kind = req.get("kind")
    code = req.get("code", "")
    if kind in ("init", "exec"):
        exec(code, ns)  # noqa: S102
        return None
    if kind == "eval":
        return eval(code, ns)  # noqa: S307
    raise ValueError("unknown kind: %r" % (kind,))


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            rid = req.get("id")
        except Exception as e:  # malformed line — best-effort error with null id
            sys.stdout.write(
                json.dumps({"id": None, "ok": False, "result": None, "error": repr(e)})
                + "\n"
            )
            sys.stdout.flush()
            continue
        try:
            result = handle(req)
            out = {"id": rid, "ok": True, "result": result, "error": None}
        except Exception as e:  # noqa: BLE001
            out = {"id": rid, "ok": False, "result": None, "error": repr(e)}
        sys.stdout.write(json.dumps(out) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
