"""A test-only worker: after the real warm-up and hook, it does one forbidden thing.

The request names the action. Each runs inside `try`/`except`, so only the hook's `os._exit`
can stop it; a worker that survives replies `survived`. The production worker has no such path.
"""

import socket
import subprocess
import sys

from imperium_cas import worker

CODE = compile("probe = 1", "<probe>", "exec")  # compiled before the hook: `exec` alone trips it


def probe(request: dict) -> dict:
    action = request.get("action")
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
