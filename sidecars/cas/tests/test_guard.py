"""The magnitude guard: the goal 2 refusals with their estimates, the allowed cases, the function
table's guard classes, matrices, and the rule that nothing big is ever computed."""

import time
import tracemalloc

import pytest

from imperium_cas.functions import FUNCTIONS
from imperium_cas.guard import MAX_DIGITS, Est, GuardError, Mat, check, fmt_digits
from imperium_cas.parse import parse


def guard(text: str, *, expand: bool = False):
    return check(parse(text), expand=expand)


def refused(text: str, *, expand: bool = False) -> GuardError:
    started = time.perf_counter()
    with pytest.raises(GuardError) as caught:
        guard(text, expand=expand)
    assert time.perf_counter() - started < 0.1, "the guard must refuse without computing"
    return caught.value


def allowed(text: str, *, expand: bool = False):
    started = time.perf_counter()
    result = guard(text, expand=expand)
    assert time.perf_counter() - started < 0.1
    return result


# --- goal 2: each of these is refused, with its estimate, and nothing is computed -----------


@pytest.mark.parametrize(
    ("text", "digits_at_least", "phrase"),
    [
        ("10^10^10", 1e10, "1e10"),
        ("1e999999999", 9e8, "1e9"),
        ("1e-999999999", 9e8, "1e9"),
        ("sqrt(2)^(10^6)", 150_000, "150,516"),
        ("factorial(factorial(20))", 1e19, "e20"),
        ("factorial(10^7)", 6e7, "6.6e7"),
        ("[[2,1],[1,1]]^(10^6)", 1e6, "1.3e6"),
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
    assert allowed("(x+y+z)^5000", expand=True) is None
    refused("(x+y+z)^10001", expand=True)
    refused("(x+y+z)^(10^5)", expand=True)
    assert allowed("(x+y+z)^10000", expand=True) is None


def test_the_expand_rule_applies_only_to_expand():
    assert allowed("(x+y+z)^50000") is None
    assert allowed("(x+y+z)^(10^6)") is None
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
        "10^99999",
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
        "(x+1)^(10^6)",
        "x^y",
        "x^(y+10^10)",
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
    allowed("log(10^99999)")
    allowed("sin(x)^1000000")


def test_root_class_is_a_power():
    refused("sqrt(2)^(10^6)")
    refused("cbrt(2)^(10^6)")
    allowed("sqrt(2)^(10^5)")
    allowed("sqrt(10^99999)")
    allowed("cbrt(10^99999)")
    refused("root(2, 1/10^6)")  # the 1,000,000th root of 2 as a power: 2^(10^6)
    allowed("root(2, 10^7)")
    allowed("root(2, 0)")
    allowed("root(x, 3)")
    allowed("sqrt(x)^2")
    assert not allowed("sqrt(2)").exact


def test_select_class_sums_its_inputs():
    allowed("lcm(10^40000, 10^40000)")
    refused("lcm(10^60000, 10^60000)")
    refused("lcm(10^30000, 10^30000, 10^30000, 10^30000)")
    allowed("gcd(10^99990, 1)")
    allowed("floor(10^99990)")
    refused("Max(10^60000, 10^60000)")
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
    allowed("factorial(x + 10^10)")


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


def test_capped_functions_with_a_symbolic_argument_are_not_sized():
    for name in ("prime", "isprime", "harmonic", "bernoulli", "catalan"):
        assert allowed(f"{name}(x)") is None


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


def test_a_symbolic_result_has_no_estimate():
    assert allowed("x + 1") is None
    assert allowed("sin(x)*2") is None
    assert allowed("x = 1") is None


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
