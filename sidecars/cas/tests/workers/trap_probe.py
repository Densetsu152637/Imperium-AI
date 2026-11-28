"""A test-only worker: the real warm-up, limits and audit hook, plus the string trap.

The trap (`support.install_string_trap`) is installed after the warm-up and just before the
hook, so every input the call builds runs under both: a `str` reaching `sympify` or
`parse_expr` is recorded, and a `compile` or `exec` event ends the worker with `REFUSED`.
"""

import sys

from imperium_cas import worker
from support import VIOLATIONS, install_string_trap

hook = worker.install_audit_hook


def trap_then_hook() -> None:
    install_string_trap()
    hook()


worker.install_audit_hook = trap_then_hook


def probe(request: dict) -> dict:
    trapped = []
    for text in request.get("expressions", []):
        for assume in (None, {"x": ["real", "positive"]}):
            reply = worker.handle({"tool": "build", "expression": text, "assume": assume})
            if "StringToSympy" in reply.get("message", ""):
                trapped.append(text[:60])
    return {"ok": True, "violations": list(VIOLATIONS), "trapped": trapped}


if __name__ == "__main__":
    worker.run(probe, sys.argv[1:])
