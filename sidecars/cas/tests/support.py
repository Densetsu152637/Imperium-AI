"""Shared test helpers: the fuzz seed and an audit-event recorder.

`sys.addaudithook` cannot be removed, so one hook is installed when this module is first imported
and it records only while a `recording()` block is open.
"""

import os
import random
import sys
from contextlib import contextmanager


def _fuzz_seed() -> int:
    """A fixed seed in CI (the commit's), so a failure reproduces; random locally."""
    if os.environ.get("CAS_FUZZ_SEED"):
        return int(os.environ["CAS_FUZZ_SEED"])
    sha = os.environ.get("GITHUB_SHA", "")
    if sha:
        return int(sha[:8], 16)
    return random.SystemRandom().randrange(2**32)


FUZZ_SEED = _fuzz_seed()
FUZZ_EXAMPLES = int(os.environ.get("CAS_FUZZ_EXAMPLES", "2000"))

# Each input must parse and guard in under 100 ms. Shared CI runners are slow and noisy, so the
# limit is five times that there, and callers take the best of a few runs.
TIME_LIMIT = 0.1 * (5 if os.environ.get("CI") else 1)

# The audit events parse.py and guard.py may cause: none. They are pure Python over strings and
# numbers (no `compile`, `exec`, `import`, `open`, `os.*`, `subprocess.*`, `socket.*`, `ctypes.*`).
ALLOWED_EVENTS: frozenset[str] = frozenset()

_events: list[str] = []
_depth = [0]


def _hook(event: str, args: tuple) -> None:
    if _depth[0] > 0:
        _events.append(event)


sys.addaudithook(_hook)


@contextmanager
def recording():
    """Record every audit event raised inside the block into the yielded list."""
    mark = len(_events)
    _depth[0] += 1
    try:
        yield _events
    finally:
        _depth[0] -= 1
        del _events[mark:]


def run_recorded(func, *args, **kwargs):
    """Call `func`, returning `(result_or_exception, events)` without letting events leak."""
    seen: list[str] = []
    with recording() as events:
        try:
            outcome = func(*args, **kwargs)
        except Exception as exc:  # noqa: BLE001 - the caller classifies the exception
            outcome = exc
        seen.extend(events)
    return outcome, seen


# --- Stage 2a: spawning workers, the string trap and measurements ---

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src")
TESTS = os.path.dirname(os.path.abspath(__file__))
WORKER_TIMEOUT = 120  # seconds for a worker to start and answer; CI runners are slow

# The base interpreter: what production spawns (plan D3), not the venv's redirector.
BASE_PYTHON = getattr(sys, "_base_executable", None) or sys.executable

# Measurements printed in the pytest summary, so CI logs record them per OS (stage 2a's outcome).
MEASUREMENTS: list[str] = []


def worker_env() -> dict[str, str]:
    """The environment for a test worker: the package and the tests on the path, hash seed 0.

    Production workers run the base interpreter under `-I` and find the package and SymPy
    through a `.pth` file (test_pid.py checks that path in CI). Tests run the base interpreter
    too, so the PID and memory are the worker's own (on Windows the venv's `python.exe` is a
    redirector), but use `PYTHONPATH` instead, so nothing is written outside `tmp`.
    """
    import sysconfig

    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join([SRC, TESTS, sysconfig.get_paths()["purelib"]])
    env["PYTHONHASHSEED"] = "0"
    return env


def spawn_worker(*args: str, script: str | None = None):
    """Start `imperium_cas.worker` (or a test worker script) with pipes on stdin and stdout."""
    import subprocess

    target = [os.path.join(TESTS, "workers", script)] if script else ["-m", "imperium_cas.worker"]
    return subprocess.Popen(  # noqa: S603 - a fixed interpreter and module
        [BASE_PYTHON, "-B", *target, *args],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=worker_env(),
    )


class StringToSympy(AssertionError):
    """A `str` reached `sympify` or `parse_expr`, or `lambdify` was called."""


VIOLATIONS: list[str] = []


def install_string_trap() -> None:
    """For the whole session: `sympify` and `parse_expr` fail on any `str`, `lambdify` on any
    call. Every module that bound the original by name gets the wrapper too, so SymPy's own
    internal calls (`Basic.subs`, `solve`) are trapped as well. Violations are also recorded,
    in case a caller swallows the exception."""
    import sympy
    import sympy.parsing.sympy_parser as parser

    originals = {
        "sympify": sympy.sympify,
        "parse_expr": parser.parse_expr,
        "lambdify": sys.modules["sympy.utilities.lambdify"].lambdify,
    }

    def wrap(name, original):
        def trapped(*args, **kwargs):
            if name == "lambdify" or (args and isinstance(args[0], str)):
                shown = repr(args[0])[:60] if args else ""
                VIOLATIONS.append(f"{name}({shown})")
                raise StringToSympy(f"{name} called with {shown}")
            return original(*args, **kwargs)

        trapped.trapped = True
        return trapped

    wrappers = {name: wrap(name, original) for name, original in originals.items()}
    for module in list(sys.modules.values()):
        for name, original in originals.items():
            if getattr(module, name, None) is original:
                setattr(module, name, wrappers[name])
