"""Stage 3: the eleven tools of plan D6, run in process as the worker runs them.

The worker's warm-up ran in `conftest.py`. Only small inputs run here, in process; every test
whose safety depends on a refusal (a blow-up the guard refuses) is in `test_tools_limited.py`,
which runs in limited workers. `test_no_inprocess_corpus.py` checks the inputs here are small.
"""

import pytest

from imperium_cas import tools, worker


def call(tool: str, **args) -> dict:
    return worker.handle({"tool": tool, "args": args})


def ok(tool: str, **args) -> dict:
    reply = call(tool, **args)
    assert reply["ok"], reply
    return reply


def refused(tool: str, category: str, **args) -> str:
    reply = call(tool, **args)
    assert not reply["ok"] and reply["category"] == category, reply
    assert reply["message"].startswith(f"{category}: ")
    return reply["message"]


def test_the_eleven_tools():
    assert set(tools.TOOLS) == {
        "simplify",
        "expand",
        "factor",
        "solve",
        "differentiate",
        "integrate",
        "limit",
        "series",
        "matrix",
        "evaluate",
        "equal",
    }


def test_simplify_and_expand():
    assert ok("simplify", expression="sin(x)^2 + cos(x)^2")["text"] == "1"
    assert ok("simplify", expression="sqrt(x^2)", assume={"x": ["positive"]})["text"] == "x"
    assert ok("simplify", expression="sqrt(x^2)")["text"] == "sqrt(x^2)"
    reply = ok("expand", expression="(x + 1)^3")
    assert reply == {
        "ok": True,
        "text": "x^3 + 3*x^2 + 3*x + 1",
        "latex": "x^{3} + 3 x^{2} + 3 x + 1",
        "kind": "expression",
    }


def test_factor():
    reply = ok("factor", expression="360")
    assert reply["text"] == "2^3*3^2*5" and reply["factors"] == [["2", 3], ["3", 2], ["5", 1]]
    assert ok("factor", expression="-12")["text"] == "-2^2*3"
    assert ok("factor", expression="1")["text"] == "1"
    assert ok("factor", expression="x^2 - 1")["text"] == "(x - 1)*(x + 1)"
    assert ok("factor", expression="x^2 + 1")["text"] == "x^2 + 1"
    assert ok("factor", expression="x^2 + 1", gaussian=True)["text"] == "(x - I)*(x + I)"


def test_solve_states_its_domain_on_every_result():
    cases = [
        ({"equations": ["x^2 = 4"]}, "{-2, 2}", "set", "complex"),
        ({"equations": ["x^2 + 1"]}, "{-I, I}", "set", "complex"),
        ({"equations": ["x^2 + 1"], "domain": "real"}, "EmptySet", "set", "real"),
        ({"equations": ["x^2 < 4"], "domain": "real"}, "Interval.open(-2, 2)", "set", "real"),
        ({"equations": ["x + y = 2", "x - y = 0"]}, "{(1, 1)}", "solutions", "complex"),
        (
            {"equations": ["x^2 + y^2 = 1", "x = y"], "domain": "real"},
            "{(-sqrt(2)/2, -sqrt(2)/2), (sqrt(2)/2, sqrt(2)/2)}",
            "solutions",
            "real",
        ),
        ({"equations": ["x*y = 6"], "variables": ["y"]}, "{6/x}", "set", "complex"),
    ]
    for args, text, kind, domain in cases:
        reply = ok("solve", **args)
        assert (reply["text"], reply["kind"], reply["domain"]) == (text, kind, domain), args


def test_solve_drops_non_real_solutions_of_a_real_system():
    reply = ok("solve", equations=["x^2 + y^2 = -1", "x = y"], domain="real")
    assert reply["text"] == "EmptySet" and reply["domain"] == "real"


def test_a_complex_domain_inequality_is_refused():
    message = refused("solve", "math", equations=["x^2 < 4"])
    assert "reals only" in message and "domain real" in message


def test_an_unsolved_equation_is_a_condition_set_reported_as_not_solved():
    reply = ok("solve", equations=["exp(x) + x = 0"], domain="real")
    assert reply["kind"] == "unevaluated" and reply["note"] == "not solved"
    assert reply["text"].startswith("ConditionSet(") and reply["domain"] == "real"


def test_solve_refusals():
    assert "nothing to solve" in refused("solve", "math", equations=["1 = 1"])
    assert "not a valid name" in refused("solve", "parse", equations=["x"], variables=["x y"])
    assert "parse" in refused("solve", "parse", equations=["x +"])


def test_differentiate():
    assert ok("differentiate", expression="x^3", variable="x")["text"] == "3*x^2"
    assert ok("differentiate", expression="x^3", order=2)["text"] == "6*x"
    assert ok("differentiate", expression="x*y", variable="y")["text"] == "x"
    assert "give the variable" in refused("differentiate", "math", expression="x*y")
    assert "one expression" in refused("differentiate", "math", expression="x < 1")


def test_integrate():
    reply = ok("integrate", expression="2x", variable="x")
    assert reply["text"] == "x^2" and reply["note"] == "antiderivative, without + C"
    assert ok("integrate", expression="x", lower="0", upper="2")["text"] == "2"
    assert ok("integrate", expression="exp(-x^2)", lower="-oo", upper="oo")["text"] == "sqrt(pi)"
    assert "both lower and upper" in refused("integrate", "parse", expression="x", lower="0")


def test_an_integral_sympy_cannot_do_is_unevaluated():
    reply = ok("integrate", expression="exp(x^x)", variable="x")
    assert reply["kind"] == "unevaluated" and reply["text"] == "Integral(exp(x^x), x)"
    definite = ok("integrate", expression="exp(x^x)", lower="0", upper="1")
    assert definite["kind"] == "unevaluated"
    assert "numerically 2.19754443801362" in definite["note"]


def test_limit():
    assert ok("limit", expression="sin(x)/x", to="0")["text"] == "1"
    assert ok("limit", expression="(1 + 1/x)^x", to="oo")["text"] == "E"
    assert ok("limit", expression="1/x", to="0", direction="+")["text"] == "oo"
    assert ok("limit", expression="1/x", to="0", direction="-")["text"] == "-oo"
    both = ok("limit", expression="Abs(x)/x", to="0")
    assert both["text"] == "does not exist" and both["exists"] is False
    bounded = ok("limit", expression="sin(1/x)", to="0", direction="+")
    assert bounded["text"] == "does not exist" and "between -1 and 1" in bounded["note"]


def test_series():
    reply = ok("series", expression="exp(x)", order=4)
    assert reply["text"] == "1 + x + x^2/2 + x^3/6 + O(x^4)"
    assert ok("series", expression="1/(1 - x)", at="oo", order=2)["text"].startswith("-1/x")
    assert "parse" in refused("series", "parse", expression="exp(x)", order=0)


def test_matrix_operations():
    m = "[[2, 1], [1, 3]]"
    expected = {
        "value": ("[[2, 1], [1, 3]]", "matrix"),
        "det": ("5", "expression"),
        "inverse": ("[[3/5, -1/5], [-1/5, 2/5]]", "matrix"),
        "transpose": ("[[2, 1], [1, 3]]", "matrix"),
        "rank": ("2", "expression"),
        "rref": ("[[1, 0], [0, 1]]", "matrix"),
        "nullspace": ("[]", "list"),
        "eigenvalues": ("[5/2 - sqrt(5)/2, sqrt(5)/2 + 5/2]", "list"),
        "charpoly": ("lambda^2 - 5*lambda + 5", "expression"),
        "solve": ("{(1/5, 3/5)}", "solutions"),
    }
    for operation, (text, kind) in expected.items():
        reply = ok("matrix", matrix=m, operation=operation, rhs="[1, 2]")
        assert (reply["text"], reply["kind"]) == (text, kind), operation
    assert ok("matrix", matrix="[[1, 2], [2, 4]]", operation="nullspace")["text"] == "[[-2, 1]]"
    assert ok("matrix", matrix="[[2, 0], [0, 2]]", operation="eigenvalues")["text"] == "[2, 2]"
    vectors = ok("matrix", matrix="[[2, 0], [0, 3]]", operation="eigenvectors")
    assert vectors["text"] == "[[2, 1, [[1, 0]]], [3, 1, [[0, 1]]]]"
    assert ok("matrix", matrix="[[1, 2], [3, 4]]^2", operation="value")["text"] == (
        "[[7, 10], [15, 22]]"
    )
    assert "not invertible" in refused(
        "matrix", "math", matrix="[[1, 2], [2, 4]]", operation="inverse"
    )
    assert "matrix value" in refused("matrix", "math", matrix="x + 1", operation="det")


def test_evaluate():
    reply = ok("evaluate", expression="sqrt(2)", digits=30)
    assert reply["text"] == "1.41421356237309504880168872421" and reply["kind"] == "number"
    assert ok("evaluate", expression="pi*x", digits=5, at={"x": "2"})["text"] == "6.2832"
    assert ok("evaluate", expression="x + y", at={"x": "1"})["kind"] == "expression"
    assert "not a valid name" in refused("evaluate", "parse", expression="x", at={"os.system": "1"})


def test_equal_has_three_outcomes_with_a_fixed_seed():
    proved = ok("equal", left="sin(x)^2", right="1 - cos(x)^2")
    assert (proved["text"], proved["method"]) == ("true", "symbolic")
    differ = ok("equal", left="sqrt(x^2)", right="x")
    assert (differ["text"], differ["method"]) == ("false", "numeric")
    assert set(differ["point"]) == {"x"} and "differs at x = " in differ["note"]
    assert ok("equal", left="sqrt(x^2)", right="x") == differ  # the same seed, the same point
    positive = ok("equal", left="sqrt(x^2)", right="x", assume={"x": ["positive"]})
    assert positive["text"] == "true"
    # Equal at every point, but SymPy proves nothing: floor(x) + floor(x + 1/2) vs floor(2x).
    unknown = ok(
        "equal", left="floor(x) + floor(x + 1/2)", right="floor(2x)", assume={"x": ["real"]}
    )
    assert (unknown["text"], unknown["method"]) == ("unknown", "numeric")
    assert "agrees at 10 random points" in unknown["note"]
    matrices = ok("equal", left="[[x, 1]]", right="[[x, 2]]")
    assert matrices["text"] == "false"
    assert "shapes differ" in refused("equal", "math", left="[[1, 2]]", right="[[1], [2]]")
    assert "same shape" in refused("equal", "math", left="[[1, 2]]", right="x")


def test_equal_points_respect_the_assumptions():
    for flags, check in (
        (["positive"], lambda v: v > 0),
        (["negative"], lambda v: v < 0),
        (["integer", "positive"], lambda v: v.is_integer and v > 0),
        (["real"], lambda v: v.is_real),
    ):
        builder = tools.Builder({"x": flags})
        symbol = builder.symbol("x")
        rng = tools.random.Random(tools.EQUAL_SEED)
        for _ in range(20):
            assert check(tools.sample(rng, symbol)), flags


def test_contradictory_assumptions_are_a_math_error():
    message = refused("simplify", "math", expression="x", assume={"x": ["positive", "negative"]})
    assert "contradict" in message


def test_hostile_names_are_refused_with_the_schema_bypassed():
    for name in ("__class__", "os.system", "x y", "", "_x", "x__y"):
        assert refused("differentiate", "parse", expression="x", variable=name)
        assert refused("simplify", "parse", expression="x", assume={name: ["real"]})
        assert refused("solve", "parse", equations=["x"], variables=[name])
        assert refused("evaluate", "parse", expression="x", at={name: "1"})


def test_bad_argument_types_are_refused_with_the_schema_bypassed():
    assert refused("simplify", "parse", expression=5)
    assert refused("differentiate", "parse", expression="x", order=True)
    assert refused("differentiate", "parse", expression="x", order=21)
    assert refused("solve", "parse", equations="x = 1")
    assert refused("solve", "parse", equations=["x"] * 11)
    assert refused("matrix", "parse", matrix="[[1]]", operation="exec")
    assert refused("limit", "parse", expression="x", to="0", direction="up")
    reply = worker.handle({"tool": "simplify", "args": ["x"]})
    assert reply["category"] == "parse"


def test_a_parse_error_names_its_column_with_a_caret():
    message = refused("simplify", "parse", expression="x + * 2")
    assert "(column 5)" in message and message.endswith("^")


@pytest.mark.parametrize(("tool", "args"), tools.WARM_UP, ids=lambda value: str(value)[:30])
def test_every_warm_up_call_gives_a_reply(tool, args):
    reply = call(tool, **args)
    assert reply["ok"] or reply["category"] in {"math", "parse", "guard"}, reply
