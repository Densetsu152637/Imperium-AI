"""The audit hook: inside a worker, forbidden events end the process with `REFUSED`.

The allow-list itself is measured here, in process, over every table entry and the hostile
corpus after the real warm-up, and printed in the pytest summary for each OS.
"""

import sys

import pytest

from imperium_cas import worker
from imperium_cas.functions import FUNCTIONS
from support import MEASUREMENTS, recording, spawn_worker
from test_hostile import ENTRIES
from test_worker import call

FORBIDDEN = ("open", "socket", "subprocess", "compile", "exec", "import")


# Actions whose exception reaches the worker's own handling (stage 2b's crash wrapper).
ESCAPES = ("escape", "bad-str", "memory", "rlimits")


@pytest.fixture(scope="module")
def probes():
    """Start every probe worker at once: each one pays SymPy's import."""
    actions = (*FORBIDDEN, *ESCAPES, "none")
    started = {action: spawn_worker(script="audit_probe.py") for action in actions}
    results = {action: call(proc, {"action": action}) for action, proc in started.items()}
    return results


@pytest.mark.parametrize("action", FORBIDDEN)
def test_forbidden_events_end_the_worker_with_refused(probes, action):
    ready, reply, code = probes[action]
    assert ready["ready"]
    assert reply is None, f"{action} survived the hook: {reply}"
    assert code == worker.REFUSED


def test_an_exception_escaping_after_the_hook_is_crashed_not_refused(probes):
    """Printing a traceback opens the source file, which the hook would call `REFUSED`; the
    worker exits `CRASHED` instead, without a reply."""
    ready, reply, code = probes["escape"]
    assert ready["audit_hook"] is True
    assert reply is None
    assert code == worker.CRASHED


def test_an_exception_whose_str_fails_still_gets_a_reply(probes):
    _, reply, code = probes["bad-str"]
    assert reply == {"ok": False, "category": "math", "message": "math: an unexpected error"}
    assert code == worker.EXIT_OK


def test_a_memory_error_after_the_hook_is_the_memory_code(probes):
    _, reply, code = probes["memory"]
    assert reply is None
    assert code == worker.MEMORY


@pytest.mark.skipif(sys.platform == "win32", reason="rlimits are Unix")
def test_the_worker_runs_with_its_rlimits(probes):
    """Read from inside, after the hook: core files off, the CPU limit (and on Linux the
    address-space limit) as the ready frame reported."""
    ready, reply, code = probes["rlimits"]
    assert code == worker.EXIT_OK
    limits, applied = reply["limits"], ready["applied"]
    assert limits["core"] == [0, 0]
    assert limits["cpu"] == [applied["cpu_soft"], applied["cpu_hard"]]
    if sys.platform.startswith("linux"):
        assert limits["as"] == [applied["address_space"]] * 2


def test_describe_never_raises():
    class Hostile(Exception):
        def __str__(self):
            raise MemoryError

    class Huge(Exception):
        def __str__(self):
            return "x" * 10_000

    assert worker.describe(Hostile()) == "an unexpected error"
    assert worker.describe(Huge()) == "x" * 500
    assert worker.describe(ValueError("plain")) == "plain"


def test_an_allowed_call_survives(probes):
    ready, reply, code = probes["none"]
    assert reply == {"ok": True, "survived": "none"}
    assert code == worker.EXIT_OK


def corpus_inputs() -> list[str]:
    inputs = [f"{name}(9)" for name in FUNCTIONS] + [f"{name}(x)" for name in FUNCTIONS]
    inputs += [text for _, text in ENTRIES]
    return inputs + ["factorial(25000)", "prime(10^6)", "binomial(10^5, 5*10^4)", "10^99998"]


def test_the_corpus_passes_the_real_hook_in_one_worker():
    """Every table entry and corpus line, built inside one worker under the installed hook."""
    proc = spawn_worker(script="audit_probe.py")
    ready, reply, code = call(proc, {"action": "corpus", "expressions": corpus_inputs()})
    assert reply == {"ok": True, "count": len(corpus_inputs())}, (reply, code)
    assert code == worker.EXIT_OK


def test_the_allow_list_never_holds_dangerous_events():
    for event in worker.ALLOWED_EVENTS:
        assert event.split(".")[0] not in {"os", "subprocess", "socket", "ctypes", "shutil"}
        assert event not in {"compile", "exec", "import", "open", "marshal.loads"}


def test_measured_events_are_inside_the_allow_list():
    """Warm up as a worker does, then record every event over the table and the corpus."""
    previous = sys.get_int_max_str_digits()
    sys.set_int_max_str_digits(worker.MAX_INT_DIGITS)
    try:
        worker.warm_up()
        seen: dict[str, str] = {}
        for text in corpus_inputs():
            with recording() as events:
                worker.handle({"tool": "build", "expression": text})
                for event in events:
                    seen.setdefault(event, text[:40])
    finally:
        sys.set_int_max_str_digits(previous)
    MEASUREMENTS.append(f"audit events in a warmed worker ({sys.platform}): {sorted(seen)}")
    stray = {event: text for event, text in seen.items() if event not in worker.ALLOWED_EVENTS}
    assert not stray, f"events outside the allow-list (first input that raised each): {stray}"
