"""The table builder: every table entry builds, names are checked twice, the walk is iterative."""

import pytest
from sympy import (
    E,
    Eq,
    I,
    Integer,
    Lt,
    Matrix,
    Rational,
    Symbol,
    Tuple,
    factorial,
    pi,
    sin,
    true,
)

from imperium_cas.build import (
    ASSUMPTIONS,
    BUILDERS,
    Builder,
    BuildError,
    MathError,
    text,
)
from imperium_cas.functions import FUNCTIONS
from imperium_cas.guard import GuardError
from imperium_cas.parse import BinOp, Call, Const, Num, Seq, Sym, parse

x, y = Symbol("x"), Symbol("y")

# A known value for every table entry (each one's smallest sensible input).
TABLE_VALUES = {
    "sin(0)": 0,
    "cos(0)": 1,
    "tan(0)": 0,
    "cot(pi/4)": 1,
    "sec(0)": 1,
    "csc(pi/2)": 1,
    "asin(1)": pi / 2,
    "acos(1)": 0,
    "atan(1)": pi / 4,
    "acot(1)": pi / 4,
    "atan2(1, 1)": pi / 4,
    "sinh(0)": 0,
    "cosh(0)": 1,
    "tanh(0)": 0,
    "coth(x)": None,
    "asinh(0)": 0,
    "acosh(1)": 0,
    "atanh(0)": 0,
    "exp(0)": 1,
    "log(8, 2)": 3,
    "ln(E)": 1,
    "erf(0)": 0,
    "Abs(-3)": 3,
    "sign(-2)": -1,
    "re(3 + 4I)": 3,
    "im(3 + 4I)": 4,
    "arg(1)": 0,
    "conjugate(I)": -I,
    "sqrt(4)": 2,
    "cbrt(27)": 3,
    "root(8, 3)": 2,
    "floor(5/2)": 2,
    "ceiling(5/2)": 3,
    "Min(3, 1, 2)": 1,
    "Max(3, 1, 2)": 3,
    "Mod(7, 3)": 1,
    "gcd(12, 18, 27)": 3,
    "lcm(4, 6, 10)": 60,
    "factorial(5)": 120,
    "gamma(5)": 24,
    "binomial(5, 2)": 10,
    "fibonacci(10)": 55,
    "lucas(5)": 11,
    "prime(5)": 11,
    "nextprime(10)": 11,
    "prevprime(10)": 7,
    "isprime(7)": true,
    "harmonic(2)": Rational(3, 2),
    "bernoulli(2)": Rational(1, 6),
    "catalan(4)": 14,
}


def build(text_: str, assume=None):
    return Builder(assume).expression(text_)


def test_the_builder_table_has_exactly_the_function_table_names():
    assert set(BUILDERS) == set(FUNCTIONS)
    with pytest.raises(TypeError):
        BUILDERS["eval"] = sin  # type: ignore[index]


def test_every_table_entry_is_covered_here():
    called = {parse(expression).name for expression in TABLE_VALUES}
    assert called == set(FUNCTIONS)


@pytest.mark.parametrize("expression", sorted(TABLE_VALUES))
def test_every_table_entry_builds(expression):
    value = build(expression)
    expected = TABLE_VALUES[expression]
    if expected is not None:
        assert value == expected
    assert not isinstance(value, str | int | bool)
    assert isinstance(text(value), str)


def test_numbers_are_exact():
    assert build("0.1") == Rational(1, 10)
    assert build("1.5e-3") == Rational(3, 2000)
    assert build("2E+3") == Integer(2000)
    assert build("12") == Integer(12)
    assert build(".5") == Rational(1, 2)


def test_operators_constants_and_implicit_multiplication():
    assert build("2x + 3(x+1)") == 5 * x + 3
    assert build("x^2 - x**2") == 0
    assert build("-x / y") == -x / y
    assert build("5!") == 120
    assert build("x!") == factorial(x)
    assert build("pi + E + I") == pi + E + I
    assert str(build("oo")) == "oo"


def test_relations():
    assert build("x = 1") == Eq(x, 1)
    assert build("x < 1") == Lt(x, 1)
    assert build("x != 1") != Eq(x, 1)
    with pytest.raises(MathError):
        build("I < 1")


def test_lists_and_matrices():
    assert build("[[1, 2], [3, 4]]") == Matrix([[1, 2], [3, 4]])
    assert build("[[1, 2], [3, 4]]^2") == Matrix([[7, 10], [15, 22]])
    assert build("[1, x]") == Tuple(1, x)
    with pytest.raises(MathError):
        build("[[1, 2], [3, 4]] + [[1, 2, 3], [4, 5, 6]]")


def test_sympy_errors_are_math_errors():
    for bad in ("isprime(7) + 1", "nextprime(x)", "[[1, 2], [3, 4]] * [[1, 2, 3]]"):
        with pytest.raises(MathError):
            build(bad)


def test_the_guard_runs_before_building():
    with pytest.raises(GuardError):
        build("2^(10^7)")


def test_assumptions():
    assert build("x", {"x": ["positive"]}).is_positive
    assert build("x", {"x": ["real", "integer"]}).is_integer
    assert build("x").is_positive is None
    builder = Builder({"x": ["real"]})
    assert builder.expression("x") is builder.symbol("x")
    for flag in ASSUMPTIONS:
        assert getattr(build("x", {"x": [flag]}), f"is_{flag}")


def test_contradictory_assumptions_are_a_math_error():
    with pytest.raises(MathError):
        Builder({"x": ["positive", "negative"]})
    with pytest.raises(MathError):
        Builder({"x": ["integer", "nonzero", "nonpositive", "nonnegative"]})


@pytest.mark.parametrize(
    "assume",
    [
        {"x": ["imaginary"]},
        {"x": ["__class__"]},
        {"x": "real"},
        {"x": [1]},
        ["x"],
        "x",
    ],
)
def test_bad_assume_values_are_refused(assume):
    with pytest.raises(BuildError):
        Builder(assume)


HOSTILE_NAMES = [
    "__class__",
    "__import__",
    "os.system",
    "x y",
    "x;y",
    "",
    "x\n",
    "a.b",
    "x-1",
    "_x",
    "x_",
    "x__y",
    "1x",
    "ｘ",
    "x\x00",
    1,
    None,
]


@pytest.mark.parametrize("name", HOSTILE_NAMES, ids=repr)
def test_hostile_names_are_refused_with_the_schema_bypassed(name):
    with pytest.raises(BuildError):
        Builder({name: ["real"]})  # an `assume` key
    with pytest.raises(BuildError):
        Builder().symbol(name)  # a `variable` or `variables` entry
    with pytest.raises(BuildError):
        Builder().at({name: "1"})  # an `at` key
    with pytest.raises(BuildError):
        Builder().build(Sym(name, 0))  # a tree made without the parser


def test_at_builds_a_substitution():
    assert Builder().at({"x": "1/2", "y": "pi"}) == {x: Rational(1, 2), y: pi}
    assert Builder({"x": ["positive"]}).at({"x": "1"}) == {Symbol("x", positive=True): 1}
    with pytest.raises(BuildError):
        Builder().at({"x": 1})
    with pytest.raises(BuildError):
        Builder().at([("x", "1")])
    with pytest.raises(GuardError):
        Builder().at({"x": "2^(10^7)"})


def test_trees_made_without_the_parser_are_checked():
    one = Num(1, 0, 0)
    for tree in (
        Call("eval", (one,), 0),
        Call("sympify", (one,), 0),
        Call("sin", (), 0),
        Call("sin", (one, one), 0),
        Call("sin", (Seq((one, one), 0),), 0),
        Const("S", 0),
        BinOp("@", one, one, 0),
        object(),
    ):
        with pytest.raises(BuildError):
            Builder().build(tree)


def test_deep_trees_build_without_recursion():
    long_sum = "+".join(["1"] * 2000)
    assert build(long_sum) == 2000
    # A valid tree about 4,000 levels deep. The guard refuses it (a factorial of an unbounded
    # value), so it is built directly: the builder must not depend on the guard for depth.
    chain = Builder().build(parse("x" + "!" * 4000))
    assert chain.func == factorial
    with pytest.raises(MathError):
        text(chain)  # printing it recurses; that is a math error, not a crash
