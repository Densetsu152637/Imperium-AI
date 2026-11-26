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


@pytest.fixture(scope="module")
def probes():
    """Start every probe worker at once: each one pays SymPy's import."""
    started = {action: spawn_worker(script="audit_probe.py") for action in (*FORBIDDEN, "none")}
    results = {action: call(proc, {"action": action}) for action, proc in started.items()}
    return results


@pytest.mark.parametrize("action", FORBIDDEN)
def test_forbidden_events_end_the_worker_with_refused(probes, action):
    ready, reply, code = probes[action]
    assert ready["ready"]
    assert reply is None, f"{action} survived the hook: {reply}"
    assert code == worker.REFUSED


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
