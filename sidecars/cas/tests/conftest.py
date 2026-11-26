import faulthandler
import os
import sys

import pytest
from hypothesis import HealthCheck, settings

from imperium_cas import worker
from support import FUZZ_EXAMPLES, FUZZ_SEED, MEASUREMENTS, install_string_trap

settings.register_profile(
    "cas",
    max_examples=FUZZ_EXAMPLES,
    deadline=None,
    database=None,
    suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large],
)
settings.load_profile("cas")

# The test run itself is capped, so a runaway test fails the run instead of the machine: on
# Windows the pytest process and every process it starts share a Job Object with a total memory
# cap; on Linux the pytest process's address space is capped (its workers inherit the cap and set
# their own lower limits); on macOS a thread polls the process's footprint.
# `CAS_TEST_MEMORY_GIB` sets the cap (default 4; 0 turns it off).
TEST_MEMORY = int(float(os.environ.get("CAS_TEST_MEMORY_GIB", "4")) * 2**30)
if TEST_MEMORY and sys.platform == "win32":
    from imperium_cas.limits_windows import cap_this_process

    TEST_JOB = cap_this_process(TEST_MEMORY)  # the handle stays open for the whole run
elif TEST_MEMORY and sys.platform.startswith("linux"):
    import resource

    _hard = resource.getrlimit(resource.RLIMIT_AS)[1]
    _soft = TEST_MEMORY if _hard == resource.RLIM_INFINITY else min(TEST_MEMORY, _hard)
    resource.setrlimit(resource.RLIMIT_AS, (_soft, _hard))
elif TEST_MEMORY and sys.platform == "darwin":
    # macOS ignores RLIMIT_AS: a thread polls this process's footprint, as the pool polls each
    # worker's, and ends the run when it is over the cap (workers have their own poll).
    import threading

    from imperium_cas.limits_macos import usage

    def _watch_footprint() -> None:
        while True:
            used = usage(os.getpid())
            if used is not None and used[0] > TEST_MEMORY:
                message = f"cas tests: footprint {used[0] >> 20} MiB over the cap\n"
                os.write(2, message.encode())
                os._exit(70)
            threading.Event().wait(0.5)

    threading.Thread(target=_watch_footprint, name="footprint cap", daemon=True).start()

# Plan D1 point 4: no `str` reaches `sympify` or `parse_expr`, and nothing calls `lambdify`, for
# the whole test session (test_no_string_to_sympy.py checks the trap and runs the corpus).
# The worker's warm-up runs first, as in a worker (where it runs before the audit hook): some of
# SymPy's lazily built tables call `sympify` on fixed strings in SymPy's own source, not on input
# (`sympy.physics.units` builds its prefixes from `sympify("yotta")` when `simplify` first runs,
# and the Meijer G table holds `S('3/2')`). The trap then holds for every test.
_previous_digits = sys.get_int_max_str_digits()
sys.set_int_max_str_digits(worker.MAX_INT_DIGITS)
worker.warm_up()
sys.set_int_max_str_digits(_previous_digits)

install_string_trap()


def pytest_report_header(config):
    return f"cas fuzz seed: {FUZZ_SEED} (CAS_FUZZ_SEED repeats it), examples: {FUZZ_EXAMPLES}"


def pytest_terminal_summary(terminalreporter):
    if MEASUREMENTS:
        terminalreporter.section("cas measurements")
        for line in MEASUREMENTS:
            terminalreporter.write_line(line)


# Every test runs under a watchdog: a test still running after this many seconds (an in-process
# corpus or fuzz run that hangs) dumps every thread's traceback and ends the whole run with a
# failure, instead of waiting for the CI job's timeout. Workers have their own watchdogs.
TEST_TIMEOUT = 300


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_call(item):
    faulthandler.dump_traceback_later(TEST_TIMEOUT, exit=True, file=sys.stderr)
    try:
        yield
    finally:
        faulthandler.cancel_dump_traceback_later()
