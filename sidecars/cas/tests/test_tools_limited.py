"""Stage 3: the tools' refusals of blow-ups, checked in limited workers, never in process.

These inputs are huge if a refusal ever regresses (`(x+y+z)^5000` has 12.5 million terms), so
each runs in a worker under its CPU, memory and wall-clock limits (a Job Object on Windows): a
regression fails the test with `limit` instead of exhausting the machine.
"""

import pytest

from imperium_cas.pool import Limits, Pool
from support import make_spawner

SPELLINGS = (  # the same exponent, 5000, written six more ways
    "5000",
    "5000.0",
    "(10000/2)",
    "(10^4/2)",
    "Abs(-5000)",
    "floor(5000.5)",
    "sqrt(25000000)",
)


@pytest.fixture(scope="module")
def pool():
    made = Pool(make_spawner(), Limits(cpu_seconds=5, memory_mib=512, wall_seconds=20))
    yield made
    made.close()


def call(pool, tool: str, **args) -> dict:
    return pool.call({"tool": tool, "args": args})


def refused(pool, tool: str, category: str, **args) -> str:
    reply = call(pool, tool, **args)
    assert not reply["ok"] and reply["category"] == category, (tool, args, reply)
    return reply["message"]


def test_guard_refusals_of_huge_results(pool):
    assert "guard" in refused(pool, "expand", "guard", expression="(x + y)^20000")
    assert "100 digits" in refused(pool, "factor", "math", expression="10^100 + 1")


def test_a_result_over_the_output_cap_is_an_error_not_cut(pool):
    message = refused(pool, "expand", "math", expression="(x + y + z)^50")
    assert "over the 64 KiB limit" in message and "evaluate" in message


@pytest.mark.parametrize("power", SPELLINGS)
def test_obvious_blow_ups_are_refused_before_sympy_starts(pool, power):
    """The guard's expansion estimate (a heuristic first line; the limits are the backstop),
    whatever the spelling of the exponent."""
    base = "(x + y + z)^" + power
    for tool, args in (
        ("simplify", {"expression": base}),
        ("expand", {"expression": base}),
        ("factor", {"expression": base + " - 1"}),
        ("solve", {"equations": [base + " = 1"], "variables": ["x"]}),
        ("integrate", {"expression": base, "variable": "x"}),
        ("series", {"expression": "exp(x) * " + base, "variable": "x"}),
        ("limit", {"expression": base, "variable": "x", "to": "0"}),
        ("equal", {"left": base, "right": "x"}),
        ("matrix", {"matrix": "[[" + base + "]]", "operation": "det"}),
    ):
        message = refused(pool, tool, "guard", **args)
        assert "terms, over the limit of 10,000" in message, (tool, message)


def test_expand_refuses_a_form_far_over_the_cap(pool):
    assert "characters" in refused(pool, "expand", "guard", expression="(x + y)^1000")
    assert call(pool, "expand", expression="(x + y)^100")["kind"] == "expression"


def test_differentiate_does_not_expand(pool):
    reply = call(pool, "differentiate", expression="(x + y + z)^5000", variable="x")
    assert reply["text"] == "5000*(x + y + z)^4999", reply


def test_matrix_operations_whose_cost_grows_too_fast_are_refused(pool):
    rows = ("[" + ", ".join(f"a{i}{j}" for j in range(9)) + "]" for i in range(9))
    symbolic = "[" + ", ".join(rows) + "]"
    assert "at most 8 rows" in refused(pool, "matrix", "math", matrix=symbolic, operation="det")
    small = "[[a, b, c, d, e], [b, a, 0, 0, 0], [c, 0, a, 0, 0], [d, 0, 0, a, 0], [e, 0, 0, 0, a]]"
    message = refused(pool, "matrix", "math", matrix=small, operation="eigenvalues")
    assert "at most 4 rows" in message
    numbers = (
        "["
        + ", ".join("[" + ", ".join(str((i * j) % 7) for j in range(12)) + "]" for i in range(12))
        + "]"
    )
    message = refused(pool, "matrix", "math", matrix=numbers, operation="eigenvalues")
    assert "at most 10 rows" in message
    assert call(pool, "matrix", matrix=numbers, operation="rank")["text"].isdigit()
