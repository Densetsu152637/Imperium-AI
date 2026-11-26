"""A test-only worker: the real warm-up and limits, but a recording audit hook instead of the
exiting one. It runs the requested calls and reports the first event outside the allow-list with
the Python stack that raised it (file names and line numbers from the frames; nothing is opened).
"""

import sys

from imperium_cas import worker

SEEN: list = []


def record(event: str, args: tuple) -> None:
    if event in worker.ALLOWED_EVENTS or SEEN:
        return
    stack = []
    frame = sys._getframe(1)
    while frame is not None and len(stack) < 40:
        stack.append(f"{frame.f_code.co_filename}:{frame.f_lineno} {frame.f_code.co_name}")
        frame = frame.f_back
    SEEN.append({"event": event, "stack": stack})


def install_recording_hook(allowed=worker.ALLOWED_EVENTS) -> None:
    sys.addaudithook(record)
    worker.STATE["audit_hook"] = True


worker.install_audit_hook = install_recording_hook


def probe(request: dict) -> dict:
    for name, args in request.get("calls", []):
        worker.handle({"tool": name, "args": args})
    return {"ok": True, "seen": SEEN[:1]}


if __name__ == "__main__":
    worker.run(probe, sys.argv[1:])
