"""A single-use worker (plan D3): warm up, lock down, answer one call, exit.

Started by the front as `python -I -B -m imperium_cas.worker [--cpu-seconds N]
[--memory-mib N]` (a missing limit takes D3's default: 15 s and 1 GiB). In order it:

1. reads its limits from the command line;
2. warms up: imports SymPy through `build` and builds every table entry and the shapes the tools
   use, so no lazy import is left for the call; integer printing is capped at 100,000 digits
   first;
3. applies its limits (`limits_posix.apply`, Unix): `RLIMIT_CORE` = 0, `RLIMIT_CPU` and, on
   Linux, `RLIMIT_AS`, each from the post-warm-up baseline;
4. installs the audit hook: any event outside `ALLOWED_EVENTS` ends the process at once with
   `os._exit(REFUSED)`. It never raises, because SymPy's own `try`/`except` could swallow it;
5. writes a ready frame (which reports the hook and the applied limits), reads one request frame
   from stdin (at most 1 MiB, the prefix checked before anything is allocated; the reader first
   checks that the hook is installed), writes one reply frame (at most 256 KiB) and exits with
   `os._exit`, so no interpreter shutdown runs under the hook.

From the hook on, nothing may raise out of `run`: printing a traceback opens the source file,
which the hook would report as `REFUSED`. So every exception after the hook ends the worker with
its own exit code: `MEMORY` for `MemoryError`, `CRASHED` for anything else.

Which limits hold where:

- **Linux:** CPU and memory are applied here (`RLIMIT_CPU`, `RLIMIT_AS`) and core files are
  off; the front's wall clock kills the worker.
- **macOS:** CPU is applied here (`RLIMIT_CPU`) and core files are off. Memory is only
  requested here, because macOS does not enforce `RLIMIT_AS`: the front polls the footprint and
  kills the worker over the limit (`pool.py`, `limits_macos.py`). The wall clock is the front's.
- **Windows:** nothing is applied here; both limits are only requested on the command line.
  Stage 5's broker applies them through the worker's Job Object.

**The worker must not be spawned in production without these limits:** on Unix only through
`pool.PosixSpawner`, which always passes them and enforces the wall clock (and, on macOS, the
memory poll); on Windows only through the broker (stage 5), never through `PosixSpawner`.

The front maps the exit status (`pool.py`): `MEMORY` and `CPU` (or the hard limit's `SIGKILL`)
are `limit`, `REFUSED` is `refused`, and `FRAME`, `CRASHED` and anything else are `crashed`.
The codes are defined in `framing.py`, which the front shares.
"""

import os
import sys
import time

from . import nocodegen
from .build import Builder, MathError, text
from .framing import (
    CPU,
    CRASHED,
    EXIT_OK,
    FRAME,
    LIMITS,
    MEMORY,
    REFUSED,
    REPLY_CAP,
    REQUEST_CAP,
    USAGE,
    FrameError,
    encode_frame,
    read_frame,
    write_frame,
)
from .functions import FUNCTIONS
from .parse import CasInputError, render_error
from .tools import TOOLS, run_tool
from .tools import WARM_UP as TOOL_WARM_UP

if sys.platform != "win32":
    from . import limits_posix

MAX_INT_DIGITS = 100_000

# Audit events a warmed-up worker may raise while it handles a call, measured over the hostile
# corpus and every table entry (stage 2a) and over the eleven tools (stage 3,
# tests/test_audit_hook.py): only `array.__new__`, which SymPy's prime sieve raises each time it
# grows (`factorial(25000)`, `prime(10^6)`). `compile`, `exec`, `import`, `open`, `os.*`,
# `subprocess.*`, `socket.*` and `ctypes.*` must never be added.
ALLOWED_EVENTS: frozenset[str] = frozenset({"array.__new__"})

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
    """`--name value` pairs from `LIMITS`, defaults filled in; anything else is a usage error."""
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
    for name, (_, _, default) in LIMITS.items():
        limits.setdefault(name, default)
    return limits


def build_call(request: dict) -> dict:
    """Parse, guard and build one expression and return its text: the worker's own check call
    (stage 2a), which tests use. The front never sends it; it offers only the eleven tools."""
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


def tool_call(request: dict) -> dict:
    """One of the eleven tools: `{"tool": name, "args": {...}}`."""
    name, args = request["tool"], request.get("args")
    try:
        return run_tool(name, args)
    except CasInputError as exc:
        return {"ok": False, "category": exc.category, "message": render_error(exc, exc.source)}
    except MathError as exc:
        return {"ok": False, "category": "math", "message": f"math: {exc.message}"}


HANDLERS = {"build": build_call, **dict.fromkeys(TOOLS, tool_call)}


def handle(request: dict) -> dict:
    tool = request.get("tool")
    handler = HANDLERS.get(tool) if isinstance(tool, str) else None
    if handler is None:
        return {"ok": False, "category": "parse", "message": "parse: unknown tool"}
    return handler(request)


def warm_up() -> None:
    """Run every code path a call can take once, so nothing is imported lazily under the hook.

    First, SymPy's polynomial rings stop generating code at run time (`nocodegen`)."""
    nocodegen.install()
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
    for name, args in TOOL_WARM_UP:
        handle({"tool": name, "args": args})
    encode_frame({"ok": True, "text": "x"}, REPLY_CAP)


# Set once the hook is installed; the request reader refuses to read without it.
STATE = {"audit_hook": False}


def refusal_frame(event) -> bytes:
    """The reply frame the hook writes before it exits: the refused event's name, at most 80
    printable ASCII characters."""
    name = event[:80] if isinstance(event, str) else "?"
    name = "".join(ch if " " <= ch < "\x7f" else "?" for ch in name)
    return encode_frame({"ok": False, "category": "refused", "event": name}, REPLY_CAP)


def install_audit_hook(allowed: frozenset[str] = ALLOWED_EVENTS) -> None:
    """Install the allow-list hook. It cannot be removed, and it exits rather than raises.

    `os._exit` and the exit code are bound as defaults, so rebinding `os` or the module's names
    later cannot change what the hook does. Before exiting it writes one reply frame naming the
    event (only its name, which is Python's or SymPy's, never input), straight to the stdout
    file descriptor, so the front can say which event was refused.
    """

    def hook(
        event: str,
        args: tuple,
        allowed=allowed,
        exit=os._exit,
        write=os.write,
        status=REFUSED,
        frame=refusal_frame,
    ) -> None:
        if event not in allowed:
            try:
                write(1, frame(event))
            except BaseException:  # noqa: BLE001, S110 - exiting matters, the report does not
                pass
            exit(status)

    sys.addaudithook(hook)
    STATE["audit_hook"] = True


def exit_on_cpu_limit(signum, frame, exit=os._exit, status=CPU) -> None:
    """The `SIGXCPU` handler: the soft CPU limit was reached."""
    exit(status)


def apply_limits(limits: dict[str, int]) -> dict:
    """Apply the limits this OS lets the worker apply itself, and say which (see the module)."""
    if sys.platform == "win32":
        return {"cpu": "requested (broker)", "memory": "requested (broker)"}
    applied = limits_posix.apply(limits["--cpu-seconds"], limits["--memory-mib"], exit_on_cpu_limit)
    applied["cpu"] = "rlimit"
    applied["memory"] = "front poll" if sys.platform == "darwin" else "rlimit"
    return applied


def read_request(stdin) -> dict:
    """Read the one request, but only once the hook is installed."""
    if not STATE["audit_hook"]:
        os._exit(CRASHED)
    try:
        return read_frame(stdin, REQUEST_CAP)
    except FrameError:
        os._exit(FRAME)


def describe(exc: BaseException) -> str:
    """At most 500 characters of `str(exc)`, or a fixed text when even that fails."""
    try:
        message = str(exc)
        return message[:500] if isinstance(message, str) else "an unexpected error"
    except BaseException:  # noqa: BLE001 - a hostile or huge `__str__` must not escape
        return "an unexpected error"


def answer(handler, stdin, stdout) -> None:
    """Read the call, answer it and write the reply. `run` catches whatever this raises."""
    request = read_request(stdin)
    try:
        reply = handler(request)
        frame = encode_frame(reply, REPLY_CAP)
    except MemoryError:
        raise
    except FrameError:
        frame = encode_frame(
            {"ok": False, "category": "math", "message": "math: the result is over 256 KiB"},
            REPLY_CAP,
        )
    except Exception as exc:  # noqa: BLE001 - an unexpected SymPy error is still one reply
        frame = encode_frame(
            {"ok": False, "category": "math", "message": f"math: {describe(exc)}"}, REPLY_CAP
        )
    stdout.write(frame)
    stdout.flush()


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
    applied = apply_limits(limits)
    limits_seconds = time.perf_counter() - started - warmup_seconds
    stdin, stdout = sys.stdin.buffer, sys.stdout.buffer
    install_audit_hook()
    try:
        ready = {
            "ready": True,
            "pid": os.getpid(),
            "warmup_seconds": round(warmup_seconds, 3),
            "limits_seconds": round(limits_seconds, 4),
            "limits": limits,
            "applied": applied,
            "audit_hook": STATE["audit_hook"],
        }
        write_frame(stdout, ready, REPLY_CAP)
        answer(handler, stdin, stdout)
    except MemoryError:
        os._exit(MEMORY)
    except BaseException:  # noqa: BLE001 - a traceback would open files under the hook
        os._exit(CRASHED)
    os._exit(EXIT_OK)


def main() -> None:
    run(handler=handle, argv=sys.argv[1:])


if __name__ == "__main__":
    main()
