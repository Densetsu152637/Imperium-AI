from hypothesis import HealthCheck, settings

from support import FUZZ_EXAMPLES, FUZZ_SEED

settings.register_profile(
    "cas",
    max_examples=FUZZ_EXAMPLES,
    deadline=None,
    database=None,
    suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large],
)
settings.load_profile("cas")


def pytest_report_header(config):
    return f"cas fuzz seed: {FUZZ_SEED} (CAS_FUZZ_SEED repeats it), examples: {FUZZ_EXAMPLES}"
