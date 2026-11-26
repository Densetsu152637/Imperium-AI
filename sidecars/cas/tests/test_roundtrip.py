"""Stage 3: a result's text is D1's language (plan D6): it parses back to an equal value.

A set of small calls over every tool is run in process, and each result is parsed and built
again (with the call's assumptions) and compared with the SymPy value the tool printed. The
corpus runs only in limited workers (Job Objects on Windows): through `simplify`, then `equal`
of the input and the result's text. Expressions, numbers and matrices round-trip whole, and a
series without its `O()` term. Sets and lists are not part of the input language, so for those
only the kind is checked. A result can hold a SymPy function outside D1's table (the derivative
of `x!` holds `polygamma`); such a text does not parse, and the calls here avoid them.
"""

import sys
import threading

import pytest
from sympy import Abs, MatrixBase, Rational, Rel, simplify
from sympy.logic.boolalg import Boolean

from imperium_cas import format as printer
from imperium_cas import tools, worker
from imperium_cas.build import Builder
from imperium_cas.guard import check as guard_check
from imperium_cas.guard import check_expansion
from imperium_cas.parse import CasInputError, parse
from imperium_cas.pool import Limits, Pool
from support import make_spawner
from test_hostile import ENTRIES

ROUND_TRIPPED = {"expression", "number", "matrix"}

CALLS = [
    ("simplify", {"expression": "(x^2 - 1)/(x - 1)"}),
    ("simplify", {"expression": "sqrt(8) + 2^(1/3) * 2^(2/3)"}),
    ("expand", {"expression": "(x - 2y)^4"}),
    ("factor", {"expression": "x^3 - x"}),
    ("factor", {"expression": "720"}),
    ("differentiate", {"expression": "atan(x) * exp(-x) / x", "variable": "x"}),
    (
        "differentiate",
        {"expression": "Abs(x) + sinh(x) + log(x)", "assume": {"x": ["positive"]}, "variable": "x"},
    ),
    ("integrate", {"expression": "x^2 * sin(x)", "variable": "x"}),
    ("integrate", {"expression": "1/(x^2 + 4)", "variable": "x", "lower": "0", "upper": "2"}),
    ("limit", {"expression": "(sqrt(x + 4) - 2)/x", "variable": "x", "to": "0"}),
    ("series", {"expression": "log(1 + x)", "variable": "x", "order": 6}),
    ("series", {"expression": "sin(x)/x", "variable": "x", "at": "1", "order": 3}),
    ("matrix", {"matrix": "[[1, x], [x, 1]]", "operation": "inverse"}),
    ("matrix", {"matrix": "[[1, x], [x, 1]]", "operation": "det"}),
    ("matrix", {"matrix": "[[1, 2], [3, 4]]", "operation": "charpoly"}),
    ("evaluate", {"expression": "E * pi", "digits": 40}),
    ("evaluate", {"expression": "sqrt(-2) + x", "digits": 12, "at": {"x": "1/7"}}),
    ("evaluate", {"expression": "[[pi, 1/3]]", "digits": 8}),
    ("simplify", {"expression": "sqrt(x^2) * y", "assume": {"x": ["negative"], "y": ["real"]}}),
]


@pytest.fixture
def printed(monkeypatch):
    """Run a tool and return its reply with the SymPy value it printed."""

    def run(tool: str, args: dict):
        seen = []

        def capture(value, kind, **extra):
            seen.append(value)
            return printer.result(value, kind, **extra)

        monkeypatch.setattr(tools, "result", capture)
        reply = worker.handle({"tool": tool, "args": args})
        monkeypatch.undo()
        return reply, (seen[-1] if seen else None)

    return run


def assert_round_trips(tool: str, args: dict, reply: dict, value) -> None:
    assert reply["ok"], reply
    if reply["kind"] not in ROUND_TRIPPED or value is None:
        assert reply["kind"] in {"set", "solutions", "list", "boolean", "unevaluated"} or (
            tool == "factor"  # an integer's factorisation is printed by hand: checked below
        ), reply
        if tool == "factor" and value is None:
            again = Builder().expression(reply["text"])
            assert again == Builder().expression(args["expression"])
        return
    text = reply["text"]
    if tool == "series":
        text, _, _ = text.partition(" + O(")
        value = value.removeO()
    if isinstance(value, Boolean):  # `true`, `false` from a constant relation: printed, not D1
        return
    again = Builder(args.get("assume")).expression(text)
    if isinstance(value, Rel):  # a relation (a corpus line such as `x < 1`)
        assert type(again) is type(value), text
        sides = ((again.lhs, value.lhs), (again.rhs, value.rhs))
        assert all(Abs(simplify(a - b)).evalf(10) < Rational(1, 10**6) for a, b in sides), text
        return
    if tool == "evaluate" and reply["kind"] == "expression":
        # Decimals beside symbols: equal at a sample point, to the printed digits.
        point = {symbol: Rational(37, 100) for symbol in value.free_symbols}
        gap = Abs((again - value).evalf(30, subs=point))
        assert gap < Rational(1, 10 ** (reply["digits"] - 2)), text
        return
    if reply["kind"] == "number":
        # The decimal parses to an exact rational, equal to the value to its printed digits.
        digits = reply["digits"]
        gap = Abs(again - value).evalf(digits + 10)
        assert gap <= Abs(value).evalf(digits + 10) * Rational(1, 10 ** (digits - 1)), text
        return
    if isinstance(value, MatrixBase):
        difference = (again - value).evalf(reply.get("digits", 15)) if "digits" in reply else None
        if difference is not None:
            assert all(Abs(entry) < Rational(1, 10**6) for entry in difference), text
        else:
            assert simplify(again - value).is_zero_matrix, text
        return
    assert simplify(again - value) == 0, (text, again, value)


@pytest.mark.parametrize(("tool", "args"), CALLS, ids=lambda value: str(value)[:40])
def test_tool_results_round_trip(printed, tool, args):
    reply, value = printed(tool, args)
    assert_round_trips(tool, args, reply, value)


def accepted(text: str) -> bool:
    """Whether the parser and the guard (pure functions of the text, which never build or
    compute anything) accept a line; only those lines can give a result."""
    try:
        root = parse(text)
        guard_check(root)
        check_expansion(root)
    except CasInputError:
        return False
    return True


VALID_LINES = [text for _, text in ENTRIES if accepted(text)]


@pytest.mark.parametrize("chunk", range(2))
def test_every_corpus_result_round_trips(chunk):
    """Each corpus line the guard accepts, through `simplify` in a limited worker (the corpus is
    never built in this process), then `equal` of the input and the result's text, also in a
    worker: the text must parse back and must never be shown unequal to the input."""
    lines = VALID_LINES[chunk::2]
    limits = Limits(cpu_seconds=3, memory_mib=512, wall_seconds=6)
    pools = [Pool(make_spawner(), limits) for _ in range(2 if sys.platform == "win32" else 4)]
    results: dict[int, tuple[dict, dict | None]] = {}

    def lane(index: int) -> None:
        for position in range(index, len(lines), len(pools)):
            text = lines[position]
            reply = pools[index].call({"tool": "simplify", "args": {"expression": text}})
            check = None
            # A result longer than the input language's 16 KiB cap cannot be parsed back.
            if reply["ok"] and reply["kind"] == "expression" and len(reply["text"]) <= 16384:
                args = {"left": text, "right": reply["text"]}
                check = pools[index].call({"tool": "equal", "args": args})
            results[position] = (reply, check)

    threads = [threading.Thread(target=lane, args=(index,)) for index in range(len(pools))]
    try:
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    finally:
        for pool in pools:
            pool.close()
    checked = 0
    for position, (reply, check) in sorted(results.items()):
        text = lines[position][:60]
        assert reply["ok"] or reply["category"] in {"parse", "guard", "math", "limit"}, (
            text,
            reply,
        )
        if check is None:
            continue
        if check["ok"]:
            assert check["text"] in {"true", "unknown"}, (text, reply["text"], check)
            checked += 1
        else:
            assert check["category"] in {"limit", "math"}, (text, reply["text"], check)
    assert checked > 20  # the corpus has valid lines too
