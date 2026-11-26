import faulthandler
import sys

import pytest
from hypothesis import HealthCheck, settings

from support import FUZZ_EXAMPLES, FUZZ_SEED, MEASUREMENTS, install_string_trap

settings.register_profile(
    "cas",
    max_examples=FUZZ_EXAMPLES,
    deadline=None,
    database=None,
    suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large],
)
settings.load_profile("cas")

# Plan D1 point 4: no `str` reaches `sympify` or `parse_expr`, and nothing calls `lambdify`, for
# the whole test session (test_no_string_to_sympy.py checks the trap and runs the corpus).
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
