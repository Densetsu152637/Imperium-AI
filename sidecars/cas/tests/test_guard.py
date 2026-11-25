"""The magnitude guard: the goal 2 refusals with their estimates, the allowed cases, the function
table's guard classes, matrices, and the rule that nothing big is ever computed."""

import time
import tracemalloc

import pytest

from imperium_cas.functions import FUNCTIONS
from imperium_cas.guard import MAX_DIGITS, Est, GuardError, Mat, check, fmt_digits, size
from imperium_cas.parse import parse
from support import TIME_LIMIT


def guard(text: str, *, expand: bool = False):
    return check(parse(text), expand=expand)


def fast(func):
    """Run `func` up to three times and require the best run under the time limit."""
    best = None
    for _ in range(3):
        started = time.perf_counter()
        try:
            outcome = func()
        except GuardError as error:
            outcome = error
        elapsed = time.perf_counter() - started
        best = elapsed if best is None else min(best, elapsed)
        if best < TIME_LIMIT:
            return outcome
    raise AssertionError(f"the guard took {best:.3f}s: it must not compute")


def refused(text: str, *, expand: bool = False) -> GuardError:
    outcome = fast(lambda: guard(text, expand=expand))
    assert isinstance(outcome, GuardError), f"{text!r} was allowed"
    return outcome


def allowed(text: str, *, expand: bool = False):
    outcome = fast(lambda: guard(text, expand=expand))
    assert not isinstance(outcome, GuardError), f"{text!r} was refused: {outcome}"
    return outcome


# --- goal 2: each of these is refused, with its estimate, and nothing is computed -----------


@pytest.mark.parametrize(
    ("text", "digits_at_least", "phrase"),
    [
        ("10^10^10", 1e10, "1e10"),
        ("1e999999999", 9e8, "1e9"),
        ("1e-999999999", 9e8, "1e9"),
        ("sqrt(2)^(10^6)", 1e6, "1e6"),
        ("factorial(factorial(20))", 1e19, "e19"),
        ("factorial(10^7)", 6e7, "6.6e7"),
        ("[[2,1],[1,1]]^(10^6)", 1e6, "e6"),
        ("9^9^9", 1e8, "digits"),
        ("2^(2^30)", 1e8, "digits"),
    ],
)
def test_goal_2_inputs_are_refused_with_their_estimate(text, digits_at_least, phrase):
    error = refused(text)
    assert error.category == "guard"
    assert error.digits is not None and error.digits >= digits_at_least * 0.99
    assert phrase in error.message
    assert "limit 100,000" in error.message or "limit 1e6" in error.message


def test_a_thirty_by_thirty_symbolic_matrix_is_refused():
    rows = ", ".join("[" + ", ".join(f"x{i}_{j}" for j in range(30)) + "]" for i in range(30))
    error = refused(f"[{rows}]")
    assert "400 entries" in error.message


def test_expand_of_a_big_power_is_stopped_by_limits_not_the_guard():
    # (x+y+z)^5000 is under expand's 10,000 cap: the guard passes it and D3's limits stop it.
    assert allowed("(x+y+z)^5000", expand=True).sym
    refused("(x+y+z)^10001", expand=True)
    refused("(x+y+z)^(10^5)", expand=True)
    assert allowed("(x+y+z)^10000", expand=True).sym


def test_the_expand_rule_applies_only_to_expand():
    assert allowed("(x+y+z)^50000").sym
    refused("(x+y+z)^(10^6)")  # about 600,000 digits once the symbols count as one
    allowed("x^(10^6)")
    refused("(x+y+z)^(10^7)")


def test_a_numeric_base_is_not_an_expand_case():
    assert allowed("2^20000", expand=True).exact
    assert allowed("(1+2)^10000", expand=True).exact


# --- things that must be allowed -------------------------------------------------------------


@pytest.mark.parametrize(
    "text",
    [
        "2^1000",
        "factorial(1000)",
        "1000!",
        "10^99998",
        "2^300000",
        "(1/2)^1000",
        "2^(-1000)",
        "3^(1/2)",
        "sqrt(2)",
        "sqrt(2)^2",
        "sqrt(10^50000)",
        "exp(1000)",
        "sinh(100)",
        "pi^100",
        "E^1000",
        "x^2 + 2x + 1",
        "x^y",
        "1^(10^10)",
        "(-1)^(10^10)",
        "0^(10^10)",
        "binomial(100, 50)",
        "binomial(10^5, 3)",
        "fibonacci(1000)",
        "lucas(1000)",
        "gamma(1000)",
        "floor(10^99990)",
        "lcm(10^40000, 10^40000)",
        "Mod(10^99990, 7)",
        "[[1,2],[3,4]]^100",
        "[[x,1],[1,x]]^1000",
        "[[1,1],[1,0]]^50000",
        "1e5",
        "1.5e-3",
        "0.1 + 0.2",
        "0e999999999",
        "x = 4",
        "x < y",
    ],
)
def test_allowed(text):
    allowed(text)


def test_the_highest_allowed_factorial():
    est = allowed("factorial(25000)")
    assert est.num < MAX_DIGITS
    refused("factorial(26000)")


def test_exact_small_exponents_are_known_exactly():
    # 10^6 is computed from its parts (a handful of digits), so a matrix power by it is sized.
    assert allowed("[[1,1],[1,0]]^(10^4)").digits < MAX_DIGITS
    refused("[[1,1],[1,0]]^(10^5)")
    allowed("[[1,1],[1,0]]^(7*10^4)")
    refused("[[1,1],[1,0]]^(8*10^4)")


def test_a_huge_exponent_on_zero_one_or_minus_one_is_fine():
    for base in ("0", "1", "(-1)"):
        assert allowed(f"{base}^(10^400)") is not None
        assert allowed(f"{base}^(10^10)") is not None


def test_an_exponent_over_1e6_is_refused_on_any_other_base():
    for text in ("2^(10^7)", "x^(10^7)", "pi^(10^7)", "I^(10^10)", "oo^(10^10)", "(1/2)^(10^7)"):
        refused(text)
    allowed("x^(10^6)")


# --- the function table's guard classes ------------------------------------------------------


def test_symbolic_class():
    assert not allowed("sin(1)").exact
    allowed("exp(1000)")
    refused("exp(10^6)")
    refused("cosh(10^6)")
    refused("exp(10^400)")  # beyond float range: still refused, never an OverflowError
    allowed("atan2(1, 2)")
    assert allowed("Abs(-5)").val == (5, 1)
    assert allowed("sign(-5)").exact
    allowed("log(10^99998)")
    allowed("sin(x)^99999")
    refused("sin(x)^999990")


def test_root_class_is_a_power():
    refused("sqrt(2)^(10^6)")
    refused("cbrt(2)^(10^6)")
    allowed("sqrt(2)^(9*10^4)")
    allowed("sqrt(10^99998)")
    allowed("cbrt(10^99998)")
    refused("root(2, 1/10^6)")  # the 1,000,000th root of 2 as a power: 2^(10^6)
    allowed("root(2, 10^7)")
    allowed("root(2, 0)")
    allowed("root(x, 3)")
    allowed("sqrt(x)^2")
    assert not allowed("sqrt(2)").exact


def test_select_class_bounds_its_result():
    allowed("lcm(10^40000, 10^40000)")
    refused("lcm(10^60000, 10^60000)")
    refused("lcm(10^30000, 10^30000, 10^30000, 10^30000)")
    allowed("gcd(10^99990, 1)")
    allowed("floor(10^99990)")
    allowed("Max(10^60000, 10^60000)")  # one of its inputs, not their product
    allowed("Min(1, 2, 3)")
    allowed("Mod(7, 3)")
    allowed("floor(x)")


def test_digits_class_estimates_the_result():
    allowed("factorial(1000)")
    refused("factorial(26000)")
    refused("factorial(10^6)")
    refused("factorial(10^7)")
    refused("gamma(10^7)")
    refused("binomial(10^7, 5)")
    refused("binomial(10^6, 500000)")
    allowed("binomial(10^4, 5000)")
    allowed("fibonacci(478000)")
    refused("fibonacci(479000)")
    refused("lucas(10^7)")
    refused("factorial(factorial(20))")
    refused("factorial(factorial(10))")
    allowed("factorial(factorial(3))")
    refused("factorial(10^400)")
    allowed("factorial(x)")
    refused("factorial(x + 10^10)")  # the constant part counts


@pytest.mark.parametrize(
    ("ok", "bad"),
    [
        ("prime(10^6)", "prime(10^6 + 1)"),
        ("prime(100)", "prime(10^7)"),
        ("isprime(10^199)", "isprime(10^200)"),
        ("nextprime(10^199)", "nextprime(10^200)"),
        ("prevprime(10^199)", "prevprime(10^200)"),
        ("harmonic(10000)", "harmonic(10001)"),
        ("bernoulli(2000)", "bernoulli(2001)"),
        ("catalan(10000)", "catalan(10001)"),
    ],
)
def test_capped_class(ok, bad):
    allowed(ok)
    refused(bad)


def test_capped_functions_with_a_symbolic_argument_are_sized_as_one():
    for name in ("prime", "isprime", "harmonic", "bernoulli", "catalan"):
        est = allowed(f"{name}(x)")
        assert est.sym and not est.unb


def test_every_function_in_the_table_is_reached_by_the_guard():
    arguments = {
        1: "2",
        2: "2, 3",
    }
    for name, function in FUNCTIONS.items():
        count = max(function.min_args, 1)
        guard(f"{name}({arguments.get(count, ', '.join(['2'] * count))})")


def test_functions_do_not_take_matrices():
    for text in ("sin([1,2])", "factorial([[1,2],[3,4]])", "Max([1, 2], 3)"):
        assert "does not take" in refused(text).message


# --- matrices and lists ----------------------------------------------------------------------


def matrix(rows: int, cols: int) -> str:
    return "[" + ", ".join("[" + ", ".join(["1"] * cols) + "]" for _ in range(rows)) + "]"


@pytest.mark.parametrize(
    ("rows", "cols", "ok"),
    [
        (30, 13, True),
        (13, 30, True),
        (20, 20, True),
        (1, 30, True),
        (30, 1, True),
        (30, 14, False),
        (14, 30, False),
        (21, 20, False),
        (31, 1, False),
        (1, 31, False),
        (30, 30, False),
    ],
)
def test_matrix_size_caps(rows, cols, ok):
    if ok:
        result = allowed(matrix(rows, cols))
        assert isinstance(result, Mat)
        assert (result.rows, result.cols) == (rows, cols)
    else:
        assert "entries" in refused(matrix(rows, cols)).message


def test_a_flat_list_is_capped_at_four_hundred_entries():
    allowed("[" + ", ".join(["1"] * 400) + "]")
    refused("[" + ", ".join(["1"] * 401) + "]")


def test_matrix_power_rules():
    refused("[[2,1],[1,1]]^(10^6)")
    refused("[[1,2],[3,4]]^x")
    refused("[[1,2],[3,4]]^(1/2)")
    refused("[[1,2],[3,4]]^pi")
    refused("[[x,1],[1,x]]^1001")
    allowed("[[x,1],[1,x]]^1000")
    allowed("[[1,2],[3,4]]^(-3)")
    refused("2^[[1,2],[3,4]]")


def test_matrix_arithmetic_is_sized():
    allowed("[[1,2],[3,4]] * [[5,6],[7,8]] + 2*[[1,0],[0,1]]")
    big = "[[10^40000, 0], [0, 10^40000]]"
    allowed(big)
    refused(f"{big} * {big} * {big}")
    refused("[[1,2],[3,4]] / [[1,2],[3,4]]")
    allowed("[[1,2],[3,4]] / 2")
    assert allowed("[[x, 1], [1, 2]] * [[1], [y]]").symbolic


# --- structure, robustness and the estimate itself -------------------------------------------


def test_a_symbol_counts_as_an_exact_unit_constant():
    for text in ("x", "x + 1", "sin(x)*2", "x = 1"):
        est = allowed(text)
        assert est.sym and not est.unb
        assert size(est) < 5
    assert allowed("x").exact
    assert not allowed("3").sym


def test_a_constant_result_has_an_estimate():
    est = allowed("2^1000")
    assert isinstance(est, Est)
    assert est.exact
    assert 300 < est.mag < 302
    assert est.val is None  # 302 digits: too big to keep


def test_small_exact_values_are_kept():
    assert allowed("1/3 + 1/6").val == (1, 2)
    assert allowed("2^10").val == (1024, 1)
    assert allowed("10^6").val == (10**6, 1)
    assert allowed("-5").val == (-5, 1)
    assert allowed("0.25").val == (1, 4)
    assert allowed("1/0").val is None


def test_division_by_zero_is_not_the_guards_business():
    allowed("1/0")
    allowed("0/0")
    allowed("0^0")
    allowed("x/0")


def test_a_long_chain_is_walked_without_recursion():
    allowed("+".join(["1"] * 2000))
    allowed("*".join(["2"] * 2000))
    allowed("+".join(["x"] * 2000))


def test_the_estimate_grows_with_repeated_work_and_is_refused_at_the_threshold():
    allowed("*".join(["10^1000"] * 99))
    refused("*".join(["10^1000"] * 101))


def test_guard_errors_carry_a_column():
    error = refused("1 + 10^10^10")
    assert error.column == 7
    error = refused("x + factorial(10^7)")
    assert error.column == 5


def test_fmt_digits():
    assert fmt_digits(5) == "5"
    assert fmt_digits(150_515.2) == "150,515"
    assert fmt_digits(1e10) == "1e10"
    assert fmt_digits(2.5e20) == "2.5e20"
    assert fmt_digits(float("inf")) == "over 1e300"
    assert fmt_digits(float("nan")) == "over 1e300"


def test_nothing_is_computed():
    """The guard works on logarithms: refusing a huge power allocates almost nothing."""
    tracemalloc.start()
    try:
        for text in ("10^10^10", "1e999999999", "sqrt(2)^(10^6)", "9^9^9", "2^(2^30)"):
            refused(text)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert peak < 2_000_000


@pytest.mark.parametrize(
    "text",
    [
        "(((2^2)^2)^2)^2",
        "((2^2)^2)^2^2",
        "factorial(5)!",
        "(2^100)!",
        "2^100!",
    ],
)
def test_nested_towers(text):
    try:
        guard(text)
    except GuardError as error:
        assert error.digits is not None or error.message


# --- the security review's findings (F1, F2, F4) -----------------------------------------------

# F1: a symbol hid its constant parts from the guard.
SYMBOLS_HIDE_CONSTANTS = [
    "(2x)^(10^6)",
    "(2*x)^(10^6)",
    "(x-x+10^60)^(10^4)",
    "exp(10^6*log(2)+x-x)",
    "exp(10^6*ln(2)+x-x)",
    "(x*10^50000)^2",
    "x^(y+10^10)",
    "factorial(x + 10^10)",
]

# F2: an estimate was not an upper bound (signed logs, divisors that may be tiny, poles).
UNSOUND_ESTIMATES = [
    "2^Max(10^70,1e-70)",
    "2^lcm(Max(10^70,1e-70),3)",
    "2^Min(Max(10^70,1e-70),Max(10^70,1e-70))",
    "lcm(10^60000, 10^60000)",
    "factorial(Max(1e70,1e-70))",
    "prime(Max(1e70,1e-70))",
    "binomial(Max(1e70,1e-70),2)",
    "2^(1e70/((10^70+1)-10^70))",
    "2^floor(1e70/((pi+1e70)-1e70))",
    "2^floor(cot(1e-70))",
    "2^floor(csc(1e-70))",
    "2^floor(coth(1e-70))",
    "2^floor(tan(pi/2-1e-70))",
    "2^ceiling(sec(pi/2-1e-70))",
    "2^Mod(1e70/((pi+1e70)-1e70), 7)",
    "exp(-10^6*log(1e-99999))",
    "2^floor(gamma(1e-70))",
    "binomial(1/3,10^6)",
    "binomial(-10^5,2*10^5)",
    "root(2,1e-100)",
    "root(2,2^-300)",
    "root(2,1/factorial(20))",
    "root(2,pi)",
    "prime(floor(1e70/((pi+1e70)-1e70)))",
    "factorial(floor(tan(pi/2-1e-70)))",
    "(1e70/((pi+1e70)-1e70))^(10^6)",
]

# B1 (inverse pairs collapse), B2 (negative powers of tiny bases), B3 (untracked binomial k).
SECOND_REVIEW = [
    "2^sin(asin(10^1000))",
    "2^cos(acos(10^1000))",
    "2^cot(acot(10^1000))",
    "2^sin(pi-asin(10^1000))",
    "2^sin(pi-asin(10^1000)+x-x)",
    "2^floor(sinh(asinh(10^1000)))",
    "tan(atan(10^99000))^(10^5)",
    "tanh(atanh(10^99000))^(10^5)",
    "sin(asin(10^99000))^(10^5)",
    "2^(10*log(exp(2*10^5)))",
    "2^((1-0.99999999999999999999999999999999999999999999999999)^(-10))",
    "2^floor((pi-3.14159265358979323846264338327950288419716939937510)^(-5))",
    "2^root(10,-1/10^10)",
    "binomial(10^6, floor(500000))",
    "binomial(10^6, ceiling(500000))",
    "binomial(10^6, Max(500000, 1))",
    "binomial(10^6, gcd(500000, 500000))",
    "binomial(10^6, Min(500000, 500001))",
]

# F4: negative matrix powers are inverses.
NEGATIVE_MATRIX_POWERS = ["[[2,1],[1,1]]^(-10^5)", "[[2,1],[1,1]]^(-10^6)"]


@pytest.mark.parametrize(
    "text", SYMBOLS_HIDE_CONSTANTS + UNSOUND_ESTIMATES + SECOND_REVIEW + NEGATIVE_MATRIX_POWERS
)
def test_reviewed_inputs_are_refused(text):
    refused(text)


@pytest.mark.parametrize(
    "text",
    [
        "(x+y+z)^5000",
        "exp(x^x^x)",
        "x^(10^6)",
        "1/sin(x)^2",
        "(1/sin(x))^2",
        "tan(x)/cos(x)",
        "sqrt(tan(x))",
        "[[2,1],[1,1]]^(-10^4)",
        "[[x,1],[1,x]]^(-1000)",
        "[[2,1],[1,1]]^(-1)",
        "binomial(1/3, 100)",
        "binomial(-10, 20)",
        "gamma(1/3)",
        "2^floor(pi)",
        "2^Max(10, 1e-70)",
        "log(x)",
        "2^log(x)",
        "exp(log(10))",
    ],
)
def test_reviewed_cases_that_stay_allowed(text):
    allowed(text)


def test_unbounded_values_are_allowed_in_arithmetic_but_not_as_integers():
    for text in ("1/sin(x)", "tan(x)", "gamma(x)", "1/(pi - pi)", "log(sin(x))", "x/tan(x)"):
        assert allowed(text).unb, text
    for text in ("2^tan(x)", "factorial(tan(x))", "prime(1/sin(x))", "binomial(10, 1/sin(x))"):
        assert "not bounded" in refused(text).message or "bounded" in refused(text).message


def test_select_results_use_non_negative_magnitudes():
    est = allowed("Max(10^70, 1e-70)")
    assert est.mag >= 70
    assert allowed("lcm(10^30, 10^30)").mag >= 60


def test_an_exact_values_bound_comes_from_its_digits():
    # The divisor's magnitude is bounded by its denominator's digits, not guessed.
    est = allowed("1e70/((10^70+1)-10^70)")
    assert est.mag >= 70
    refused("2^(1e70/((10^70+1)-10^70))")


def test_negative_matrix_powers_cost_the_inverse():
    positive = allowed("[[2,1],[1,1]]^(3*10^4)").digits
    negative = allowed("[[2,1],[1,1]]^(-3*10^4)").digits
    assert negative > 1.5 * positive


@pytest.mark.parametrize(
    "text",
    [
        "sin(asin(x))",
        "2^sin(x)",
        "tan(atan(x))^2",
        "x^(-1)",
        "(x+1)^(-3)",
        "2^x",
        "binomial(5, floor(2))",
    ],
)
def test_second_review_cases_that_stay_allowed(text):
    allowed(text)


def test_an_inverse_pair_keeps_its_arguments_size():
    assert allowed("sin(asin(10^1000))").mag >= 1000
    assert allowed("tan(atan(10^1000))").num >= 1000
    assert allowed("sin(pi - asin(10^1000))").mag >= 1000
    assert allowed("log(exp(10^4))").num >= 5


def test_a_negative_power_of_a_tiny_base_is_big():
    est = allowed("(1-0.999999999999999999999999999999999999999999999999)^(-10)")
    assert est.mag >= 100 or est.unb
    assert allowed("pi^(-2)").unb  # an inexact base to a possibly negative power


def test_select_keeps_small_exact_values():
    assert allowed("floor(7/2)").val == (3, 1)
    assert allowed("ceiling(7/2)").val == (4, 1)
    assert allowed("floor(-7/2)").val == (-4, 1)
    assert allowed("Max(3, 5, 4)").val == (5, 1)
    assert allowed("Min(3, 5, 4)").val == (3, 1)
    assert allowed("gcd(12, 18)").val == (6, 1)
    assert allowed("lcm(4, 6)").val == (12, 1)
    assert allowed("Mod(7, 3)").val == (1, 1)
    assert allowed("Mod(-7, 3)").val == (2, 1)
    assert allowed("Mod(7, -3)").val == (-2, 1)


def test_the_limit_is_exclusive():
    allowed("10^99998")
    refused("10^99999")
