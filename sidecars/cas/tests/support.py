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
