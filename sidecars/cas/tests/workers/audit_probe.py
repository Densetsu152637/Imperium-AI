"""A test-only worker: after the real warm-up and hook, it does one forbidden thing.

The request names the action. Each runs inside `try`/`except`, so only the hook's `os._exit`
can stop it; a worker that survives replies `survived`. The production worker has no such path.
"""

import socket
import subprocess
import sys

from sympy import symbols
from sympy.polys.rings import ring

from imperium_cas import worker

if sys.platform != "win32":
    import resource

CODE = compile("probe = 1", "<probe>", "exec")  # compiled before the hook: `exec` alone trips it


class Escape(BaseException):
    """Not an `Exception`: it passes the handler's own `except Exception`."""


class BadStr(Exception):
    def __str__(self):
        raise RuntimeError("str() of this exception fails")


def outside_the_try(action: str) -> dict | None:
    """Actions whose exceptions must reach the worker's own handling, not this probe's."""
    if action == "escape":
        raise Escape
    if action == "bad-str":
        raise BadStr
    if action == "memory":
        raise MemoryError
    if action == "rlimits":
        kinds = {"core": resource.RLIMIT_CORE, "cpu": resource.RLIMIT_CPU}
        if hasattr(resource, "RLIMIT_AS"):
            kinds["as"] = resource.RLIMIT_AS
        return {
            "ok": True,
            "limits": {name: resource.getrlimit(kind) for name, kind in kinds.items()},
        }
    return None


def probe(request: dict) -> dict:
    action = request.get("action")
    direct = outside_the_try(action)
    if direct is not None:
        return direct
    try:
        if action == "open":
            open(__file__, "rb").close()  # noqa: SIM115
        elif action == "socket":
            socket.socket().close()
        elif action == "subprocess":
            subprocess.run([sys.executable, "-c", "0"], check=False)  # noqa: S603
        elif action == "compile":
            compile("1 + 1", "<probe>", "eval")
        elif action == "exec":
            exec(CODE, {})  # noqa: S102
        elif action == "import":
            if "mailbox" in sys.modules:
                return {"ok": False, "message": "mailbox was already imported"}
            import mailbox  # noqa: F401 - a module the worker never imported
        elif action == "ring":
            # A polynomial ring over generators never seen before: SymPy would generate its
            # monomial functions with `exec`, unless `nocodegen` is installed.
            ring(symbols("probe_a probe_b probe_c probe_d probe_e probe_f probe_g"), "QQ")
            return {"ok": True, "ring": True}
        elif action == "corpus":
            # Every expression under the real hook: an event outside the allow-list exits.
            for expression in request.get("expressions", []):
                worker.handle({"tool": "build", "expression": expression})
            return {"ok": True, "count": len(request.get("expressions", []))}
    except BaseException:  # noqa: BLE001, S110 - the point: the hook must not be swallowable
        pass
    return {"ok": True, "survived": action}


if __name__ == "__main__":
    worker.run(probe, sys.argv[1:])
