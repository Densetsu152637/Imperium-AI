"""A single-use worker (plan D3): warm up, lock down, answer one call, exit.

Started by the front as `python -I -B -m imperium_cas.worker [--cpu-seconds N]
[--memory-mib N]`. In order it:

1. reads its limits from the command line (stage 2b applies them; this stage only checks them);
2. warms up: imports SymPy through `build` and builds every table entry and the shapes the tools
   use, so no lazy import is left for the call;
3. sets `RLIMIT_CORE` to 0 (Unix); integer printing is capped at 100,000 digits before warm-up;
4. installs the audit hook: any event outside `ALLOWED_EVENTS` ends the process at once with
   `os._exit(REFUSED)`. It never raises, because SymPy's own `try`/`except` could swallow it;
5. writes a ready frame, reads one request frame from stdin (at most 1 MiB, the prefix checked
   before anything is allocated), writes one reply frame (at most 256 KiB) and exits with
   `os._exit`, so no interpreter shutdown runs under the hook.

The front maps the exit status: `REFUSED` is a `refused` error, `FRAME` and anything else that
is not a limit are `crashed`.
"""

import os
import sys
import time

from .build import Builder, MathError, text
from .framing import REPLY_CAP, REQUEST_CAP, FrameError, encode_frame, read_frame, write_frame
from .functions import FUNCTIONS
from .parse import CasInputError, render_error

if sys.platform != "win32":
    import resource

EXIT_OK = 0
USAGE = 64  # a bad command line
FRAME = 65  # a request frame over its cap, truncated or not JSON
MEMORY = 66  # a `MemoryError` while handling the call (stage 2b maps it to `limit`)
REFUSED = 77  # the audit hook saw an event outside the allow-list

MAX_INT_DIGITS = 100_000

# Audit events a warmed-up worker may raise while it handles a call, measured over the hostile
# corpus and every table entry (stage 2a, tests/test_audit_hook.py): only `array.__new__`, which
# SymPy's prime sieve raises each time it grows (`factorial(25000)`, `prime(10^6)`). Stage 3
# measures it again over the eleven tools. `compile`, `exec`, `import`, `open`, `os.*`,
# `subprocess.*`, `socket.*` and `ctypes.*` must never be added.
ALLOWED_EVENTS: frozenset[str] = frozenset({"array.__new__"})

# Command-line limits: name -> (smallest, largest). Stage 2b applies them.
LIMITS = {"--cpu-seconds": (1, 120), "--memory-mib": (64, 8192)}

WARM_UP = (
    "x + 2y - 3/4 + 0.5 + 1.5e-3",
    "(x+1)^3 * 2(x-1) / (x^2 + 1)",
    "x^2 + y^2 = 1",
    "x != 1",
    "x <= 2",
    "[[1, 2], [3, x]]^2 + [[0, 1], [1, 0]]",
    "[1, x, 2]",
    "pi + E + I + oo",
    "5! + sqrt(x)",
    "10^20000",  # printing a big integer imports CPython's `_pylong` the first time
    "factorial(3000) + prime(1000) + harmonic(50)",  # the sieve and the big-integer paths
)


def parse_limits(argv: list[str]) -> dict[str, int]:
    """`--name value` pairs from `LIMITS`; anything else is a usage error."""
    limits: dict[str, int] = {}
    if len(argv) % 2:
        raise ValueError("every limit needs a value")
    for index in range(0, len(argv), 2):
        name, raw = argv[index], argv[index + 1]
        bounds = LIMITS.get(name)
        if bounds is None or name in limits:
            raise ValueError(f"unknown or repeated option {name[:40]!r}")
        if not raw.isascii() or not raw.isdigit() or len(raw) > 6:
            raise ValueError(f"{name} needs a whole number")
        value = int(raw)
        if not bounds[0] <= value <= bounds[1]:
            raise ValueError(f"{name} must be {bounds[0]} to {bounds[1]}")
        limits[name] = value
    return limits


def build_call(request: dict) -> dict:
    """Stage 2a's one call: parse, guard and build an expression, and return its text.

    Stage 3 replaces this with the eleven tools.
    """
    expression = request.get("expression")
    if not isinstance(expression, str):
        return {"ok": False, "category": "parse", "message": "parse: expression must be a string"}
    try:
        value = Builder(request.get("assume")).expression(expression)
        return {"ok": True, "text": text(value)}
    except CasInputError as exc:
        return {"ok": False, "category": exc.category, "message": render_error(exc, expression)}
    except MathError as exc:
        return {"ok": False, "category": "math", "message": f"math: {exc.message}"}


HANDLERS = {"build": build_call}


def handle(request: dict) -> dict:
    handler = HANDLERS.get(request.get("tool"))
    if handler is None:
        return {"ok": False, "category": "parse", "message": "parse: unknown tool"}
    return handler(request)


def warm_up() -> None:
    """Run every code path a call can take once, so nothing is imported lazily under the hook."""
    for function in FUNCTIONS.values():
        args = ", ".join(["7"] * function.min_args)
        for expression in (f"{function.name}({args})", f"{function.name}(x)"):
            handle({"tool": "build", "expression": expression})
    for expression in WARM_UP:
        handle({"tool": "build", "expression": expression})
        handle({"tool": "build", "expression": expression, "assume": {"x": ["positive"]}})
    for bad in ("x +", "2^(10^7)", "I < 1", "x y"):
        handle({"tool": "build", "expression": bad})
    handle({"tool": "build", "expression": "x", "assume": {"x": ["positive", "negative"]}})
    handle({"tool": "build", "expression": "x", "assume": {"__class__": ["real"]}})
    encode_frame({"ok": True, "text": "x"}, REPLY_CAP)


def max_rss_bytes() -> int | None:
    """Peak resident memory, on Unix (Linux reports KiB, macOS bytes)."""
    if sys.platform == "win32":
        return None
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return peak if sys.platform == "darwin" else peak * 1024


def install_audit_hook(allowed: frozenset[str] = ALLOWED_EVENTS) -> None:
    """Install the allow-list hook. It cannot be removed, and it exits rather than raises."""

    def hook(event: str, args: tuple) -> None:
        if event not in allowed:
            os._exit(REFUSED)

    sys.addaudithook(hook)


def run(handler, argv: list[str]) -> None:
    """The worker's life, with `handler` answering the call. Never returns."""
    try:
        limits = parse_limits(argv)
    except ValueError as exc:
        sys.stderr.write(f"imperium_cas.worker: {exc}\n")
        sys.stderr.flush()
        os._exit(USAGE)
    sys.set_int_max_str_digits(MAX_INT_DIGITS)  # before warm-up, so it prints a big integer
    started = time.perf_counter()
    warm_up()
    warmup_seconds = time.perf_counter() - started
    if sys.platform != "win32":
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    ready = {
        "ready": True,
        "pid": os.getpid(),
        "warmup_seconds": round(warmup_seconds, 3),
        "max_rss_bytes": max_rss_bytes(),
        "limits": limits,
    }
    stdin, stdout = sys.stdin.buffer, sys.stdout.buffer
    install_audit_hook()
    write_frame(stdout, ready, REPLY_CAP)
    try:
        request = read_frame(stdin, REQUEST_CAP)
    except FrameError:
        os._exit(FRAME)
    try:
        reply = handler(request)
        frame = encode_frame(reply, REPLY_CAP)
    except MemoryError:
        os._exit(MEMORY)
    except FrameError:
        frame = encode_frame(
            {"ok": False, "category": "math", "message": "math: the result is over 256 KiB"},
            REPLY_CAP,
        )
    except Exception as exc:  # noqa: BLE001 - an unexpected SymPy error is still one reply
        frame = encode_frame(
            {"ok": False, "category": "math", "message": f"math: {str(exc)[:500]}"}, REPLY_CAP
        )
    stdout.write(frame)
    stdout.flush()
    os._exit(EXIT_OK)


def main() -> None:
    run(handle, sys.argv[1:])


if __name__ == "__main__":
    main()
