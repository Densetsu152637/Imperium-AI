"""The magnitude guard (plan D2): a bottom-up size estimate over the parsed tree.

Every node gets a sound upper bound on what SymPy could build from it, and the guard never
computes a value. A symbol counts as an exact unit constant (`1`), so the constant parts of an
expression that also holds symbols are always sized (`(2x)^(10^6)` is as big as `2^(10^6)`).

An estimate (`Est`) carries:

- `mag`: an upper bound on log10 of the absolute value, or infinity;
- `num` and `den`: a budget of digits for the numerator and denominator of any exact rational the
  value could collapse to. The budget runs through inexact values too, because `(pi+1e70)-1e70`
  collapses to `pi`, and `sqrt(2)^2` to `2`. Division by an inexact value, or any other division
  whose result cannot be bounded, still carries its budget;
- `exact`: the value is a rational number;
- `val`: the exact value as a `(p, q)` pair, kept only while it is small (at most 64 digits), so
  a small exponent such as `10^6` is known exactly;
- `sym`: the subtree contains a symbol;
- `unb`: the value is unbounded (a division by something that may be tiny, `tan` near a pole,
  `gamma` near a pole). Unbounded values are allowed in arithmetic, but refused wherever an
  integer is needed: an exponent, or an argument of `factorial`, `prime`, `binomial` and the
  other digits and capped functions.

A node whose size is over 100,000 digits is a `GuardError` that names the estimate. What the guard
cannot see is stopped by the worker's resource limits (plan D3).

The tree walk is iterative, so a long left-leaning chain (`1+1+1+...`) cannot reach Python's
recursion limit.
"""

import math
from dataclasses import dataclass

from .functions import CAPPED, DIGITS, FUNCTIONS, ROOT, SELECT, SYMBOLIC
from .parse import (
    BinOp,
    Call,
    CasInputError,
    Const,
    Neg,
    Num,
    Rel,
    Seq,
    Sym,
    children,
    postorder,
)

MAX_DIGITS = 100_000
MAX_EXPONENT = 10.0**6  # an exponent above 1e6 is refused unless the base is 0, 1 or -1
MAX_MATRIX_SIDE = 30
MAX_MATRIX_ENTRIES = 400
MAX_SYMBOLIC_MATRIX_POWER = 1000
MAX_EXPAND_EXPONENT = 10_000
MAX_GENERALIZED_BINOMIAL_K = 10**5  # binomial(n, k) with n negative or not a whole number
VALUE_DIGITS = 64  # exact values are tracked while they have at most this many digits
SMALL_FACTORIAL = 30  # factorial(n) is computed exactly up to here, to keep its value

LOG2 = math.log10(2)
LN10 = math.log(10)
FIB_DIGITS_PER_N = 0.209  # digits of fibonacci(n) and lucas(n) per unit of n, rounded up
EXP_GROWTH = 1 / LN10  # log10(e): exp(x) has about x * 0.4343 digits
HUGE = 1e300
SMALL_MAG = 0.2  # log10 of 1.58, above pi/2: the bound for functions with a value below that

# Known constants: log10 of their value. They are not exact rationals.
CONSTANT_MAG = {"pi": math.log10(math.pi), "E": math.log10(math.e), "I": 0.0, "oo": 0.0}

GROWING = frozenset({"exp", "sinh", "cosh"})
KEEP_ARGUMENT = frozenset({"Abs", "re", "im", "conjugate"})
BOUNDED = frozenset({"sin", "cos", "asin", "acos", "atan", "acot", "tanh", "erf", "arg", "atan2"})
POLES = frozenset({"tan", "cot", "sec", "csc", "coth", "atanh"})
LOGS = frozenset({"log", "ln"})
SLOW = frozenset({"asinh", "acosh"})


class GuardError(CasInputError):
    """The guard refused an input that would be too large. `digits` is the estimate, if any."""

    category = "guard"

    @property
    def digits(self) -> float | None:
        return self.args[2] if len(self.args) > 2 else None


@dataclass(frozen=True, slots=True)
class Est:
    """The estimate for a scalar subtree."""

    mag: float
    num: float
    den: float
    exact: bool
    val: tuple | None = None
    sym: bool = False
    unb: bool = False


@dataclass(frozen=True, slots=True)
class Mat:
    """The estimate for a list or matrix: its shape and the digits of its largest entry."""

    rows: int
    cols: int
    digits: float
    symbolic: bool
    matrix: bool  # two-dimensional (a list of rows)


UNIT = Est(0.0, 1.0, 1.0, True)


def unbounded(sym: bool, num: float = 1.0, den: float = 1.0) -> Est:
    return Est(math.inf, num, den, False, None, sym, True)


def refuse(message: str, pos: int, digits: float | None = None) -> GuardError:
    if digits is None:
        return GuardError(message, pos + 1)
    return GuardError(message, pos + 1, digits)


def fmt_digits(count: float) -> str:
    """A short phrase for a digit count: `150,515` or `1e10`."""
    if count != count or count >= HUGE:
        return "over 1e300"
    if count < 1_000_000:
        return f"{count:,.0f}"
    power = math.floor(math.log10(count))
    lead = f"{count / 10.0**power:.1f}".rstrip("0").rstrip(".")
    return f"{lead}e{power}" if lead != "1" else f"1e{power}"


def size(est: Est) -> float:
    """The digit count the threshold is applied to."""
    digits = max(est.num, est.den)
    if not est.unb:
        digits = max(digits, max(est.mag, 0.0) + 1)
    return digits


def too_big(digits: float, pos: int, what: str = "result") -> GuardError:
    return refuse(
        f"the {what} would have about {fmt_digits(digits)} digits (limit {MAX_DIGITS:,})",
        pos,
        digits,
    )


def digit_count(value: int) -> int:
    return len(str(abs(value)))


def keep_val(p: int, q: int) -> tuple | None:
    """Keep an exact value, reduced, only while it is small."""
    if q == 0 or digit_count(p) > VALUE_DIGITS or digit_count(q) > VALUE_DIGITS:
        return None
    if q < 0:
        p, q = -p, -q
    g = math.gcd(p, q)
    return (p // g, q // g)


def times(a: float, b: float) -> float:
    """a * b, with 0 * infinity = 0."""
    return 0.0 if a == 0 or b == 0 else a * b


def integer_value(est: Est) -> int | None:
    """The exact integer value of a constant, when it is known and an integer."""
    if est.val is not None and est.val[1] == 1:
        return est.val[0]
    return None


def abs_upper(est: Est) -> float:
    """An upper bound on the absolute value of a constant (`inf` when unbounded or huge)."""
    if est.unb:
        return math.inf
    if est.val is not None:
        return abs(est.val[0]) / est.val[1]
    bound = min(est.mag, est.num) if est.exact else est.mag
    return math.inf if bound > 300 else 10.0**bound


def num_est(node: Num) -> Est:
    m, e = node.mantissa, node.exponent
    if m == 0:
        e = 0
    elif abs(e) > 10**15:
        e = int(HUGE) if e > 0 else -int(HUGE)
    digits_m = digit_count(m)
    mag = (math.log10(m) if m > 0 else 0.0) + e
    num, den = (digits_m + e, 1) if e >= 0 else (digits_m, -e + 1)
    val = None
    if digits_m <= VALUE_DIGITS and abs(e) <= VALUE_DIGITS:
        val = keep_val(m * 10**e, 1) if e >= 0 else keep_val(m, 10**-e)
    return Est(mag, float(num), float(den), True, val)


def add_est(a: Est, b: Est, sub: bool) -> Est:
    unb = a.unb or b.unb
    mag = math.inf if unb else max(a.mag, b.mag) + LOG2
    if a.den == 1 and b.den == 1:
        num, den = max(a.num, b.num) + 1, 1.0
    else:
        num, den = max(a.num + b.den, b.num + a.den) + 1, a.den + b.den
    exact = a.exact and b.exact
    val = None
    if exact and a.val is not None and b.val is not None:
        (p1, q1), (p2, q2) = a.val, b.val
        val = keep_val(p1 * q2 + (-p2 if sub else p2) * q1, q1 * q2)
    return Est(mag, num, den, exact, val, a.sym or b.sym, unb)


def mul_est(a: Est, b: Est) -> Est:
    unb = a.unb or b.unb
    mag = math.inf if unb else a.mag + b.mag
    den = 1.0 if a.den == 1 and b.den == 1 else a.den + b.den
    exact = a.exact and b.exact
    val = None
    if exact and a.val is not None and b.val is not None:
        val = keep_val(a.val[0] * b.val[0], a.val[1] * b.val[1])
    return Est(mag, a.num + b.num, den, exact, val, a.sym or b.sym, unb)


def div_est(a: Est, b: Est) -> Est:
    num, den = a.num + b.den, a.den + b.num
    sym = a.sym or b.sym
    if b.val is not None and b.val[0] == 0:
        return Est(a.mag, num, den, a.exact, None, sym, a.unb)  # x/0 is not a number
    if a.unb or b.unb or not b.exact:
        # The divisor may be tiny: the quotient may be anything.
        return unbounded(sym, num, den)
    if b.val is not None:
        shift = math.log10(b.val[1]) - math.log10(abs(b.val[0]))
    else:
        shift = b.den  # |1/b| = q/|p| <= q < 10**den
    val = None
    if a.exact and a.val is not None and b.val is not None:
        val = keep_val(a.val[0] * b.val[1], a.val[1] * b.val[0])
    return Est(a.mag + shift, num, den, a.exact, val, sym, False)


def pow_est(base: Est, f: float, exp_int: int | None, pos: int, sym: bool) -> Est:
    """`base` raised to an exponent whose absolute value is at most `f`.

    `exp_int` is the exponent when it is a known integer.
    """
    if base.val in ((0, 1), (1, 1), (-1, 1)):
        # 0, 1 and -1 stay 0, 1 and -1 (or 0 and 1) whatever the exponent: nothing grows.
        keep = base.val[0] == 1 or (base.val[0] == 0 and exp_int is not None and exp_int > 0)
        return Est(0.0, 1.0, 1.0, True, base.val if keep else None, sym)
    whole = base.exact and base.den == 1
    if whole:
        # A whole number's own log10 is tighter than its digit count: 10^199 has 200 digits.
        estimate = f * max(base.mag, 0.0) + 1
    else:
        estimate = times(f, max(base.num, base.den))
    if f == math.inf:
        raise refuse("the exponent is not bounded: it could be huge", pos)
    if f > MAX_EXPONENT:
        raise refuse(
            f"the exponent is too large (about {fmt_digits(f)}): the result would have "
            f"about {fmt_digits(estimate)} digits (limit {MAX_DIGITS:,})",
            pos,
            estimate,
        )
    if estimate > MAX_DIGITS:
        raise too_big(estimate, pos)
    mag = math.inf if base.unb else times(abs(base.mag), f)
    exact = base.exact and exp_int is not None
    if exp_int is not None:
        n = abs(exp_int)
        if whole:
            big = math.ceil(n * max(base.mag, 0.0)) + 1.0
            num, den = (big, 1.0) if exp_int >= 0 else (1.0, big)
        elif exp_int >= 0:
            num, den = base.num * n, base.den * n
        else:
            num, den = base.den * n, base.num * n
        val = None
        if exact and base.val is not None and n <= 1000 and mag + 1 <= VALUE_DIGITS:
            p, q = base.val
            if exp_int >= 0:
                val = keep_val(p**n, q**n)
            elif p != 0:
                val = keep_val(q**n, p**n)
        return Est(mag, max(num, 1.0), max(den, 1.0), exact, val, sym, base.unb)
    num, den = max(math.ceil(f * base.num), 1.0), max(math.ceil(f * base.den), 1.0)
    return Est(mag, num, den, False, None, sym, base.unb)


def constant_est(node: Const) -> Est:
    return Est(CONSTANT_MAG[node.name], 1.0, 1.0, False)


def entry_digits(item: Est | Mat) -> float:
    """Digits of a scalar or of a matrix's largest entry."""
    return item.digits if isinstance(item, Mat) else size(item)


def is_symbolic(item: Est | Mat) -> bool:
    return item.symbolic if isinstance(item, Mat) else item.sym


def seq_est(node: Seq, kids: list) -> Mat:
    rows = [kid for kid in kids if isinstance(kid, Mat)]
    if rows:
        cols = rows[0].cols
        if (
            len(rows) > MAX_MATRIX_SIDE
            or cols > MAX_MATRIX_SIDE
            or len(rows) * cols > MAX_MATRIX_ENTRIES
        ):
            raise refuse(
                f"a matrix has at most {MAX_MATRIX_SIDE} rows and columns and "
                f"{MAX_MATRIX_ENTRIES} entries, got {len(rows)} by {cols}",
                node.pos,
            )
        digits = max(row.digits for row in rows)
        return Mat(len(rows), cols, digits, any(row.symbolic for row in rows), True)
    if len(kids) > MAX_MATRIX_ENTRIES:
        raise refuse(f"a list has at most {MAX_MATRIX_ENTRIES} entries, got {len(kids)}", node.pos)
    digits = max((entry_digits(kid) for kid in kids), default=1.0)
    return Mat(1, len(kids), digits, any(kid.sym for kid in kids), False)


def matrix_power(base: Mat, exponent, pos: int) -> Mat:
    n = integer_value(exponent) if isinstance(exponent, Est) else None
    if n is None:
        raise refuse("a matrix power needs a constant integer exponent", pos)
    count = abs(n)
    if base.symbolic and count > MAX_SYMBOLIC_MATRIX_POWER:
        raise refuse(
            f"a symbolic matrix power is limited to exponents up to {MAX_SYMBOLIC_MATRIX_POWER}",
            pos,
        )
    side = max(base.rows, base.cols, 1)
    digits = count * (base.digits + math.log10(side))
    if n < 0:
        # The inverse has entries of about `side` times the digits (Cramer), then the power.
        digits *= side
    if digits > MAX_DIGITS:
        raise too_big(digits, pos, "matrix entries")
    return Mat(base.rows, base.cols, max(digits, base.digits), base.symbolic, base.matrix)


def matrix_binop(op: str, left, right, pos: int) -> Mat:
    mats = [item for item in (left, right) if isinstance(item, Mat)]
    symbolic = is_symbolic(left) or is_symbolic(right)
    rows, cols = mats[0].rows, mats[0].cols
    if op in ("+", "-"):
        digits = max(entry_digits(left), entry_digits(right)) + 1
    elif op == "*" and len(mats) == 2:
        digits = left.digits + right.digits + math.log10(max(left.cols, 1))
        rows, cols = left.rows, right.cols
    else:  # a scalar times a matrix, or a matrix over a scalar
        digits = entry_digits(left) + entry_digits(right)
    if digits > MAX_DIGITS:
        raise too_big(digits, pos, "matrix entries")
    return Mat(rows, cols, digits, symbolic, any(mat.matrix for mat in mats))


def binop_est(node: BinOp, left, right, expand: bool):
    op, pos = node.op, node.pos
    if isinstance(right, Mat) and op == "/":
        raise refuse("a matrix cannot be a divisor", pos)
    if op == "^":
        if isinstance(left, Mat):
            return matrix_power(left, right, pos)
        if isinstance(right, Mat):
            raise refuse("a list cannot be an exponent", pos)
        return power_node(left, right, pos, expand)
    if isinstance(left, Mat) or isinstance(right, Mat):
        return matrix_binop(op, left, right, pos)
    if op == "+":
        return add_est(left, right, False)
    if op == "-":
        return add_est(left, right, True)
    if op == "*":
        return mul_est(left, right)
    return div_est(left, right)


def power_node(base: Est, exponent: Est, pos: int, expand: bool) -> Est:
    if expand and base.sym and not exponent.sym:
        bound = abs_upper(exponent)
        if bound > MAX_EXPAND_EXPONENT:
            raise refuse(
                f"expand refuses an exponent over {MAX_EXPAND_EXPONENT:,} on a symbolic base",
                pos,
            )
    sym = base.sym or exponent.sym
    return pow_est(base, abs_upper(exponent), integer_value(exponent), pos, sym)


def function_est(node: Call, kids: list) -> Est:
    name, pos = node.name, node.pos
    function = FUNCTIONS[name]
    for kid in kids:
        if isinstance(kid, Mat):
            raise refuse(f"'{name}' does not take a list or matrix", pos)
    sym = any(kid.sym for kid in kids)
    if function.guard_class == SYMBOLIC:
        return symbolic_est(name, kids, sym)
    if function.guard_class == ROOT:
        return root_est(name, kids, pos, sym)
    if function.guard_class == SELECT:
        return select_est(name, kids, sym)
    for kid in kids:
        if kid.unb:
            raise refuse(f"the argument of '{name}' is not bounded: it could be huge", pos)
    if function.guard_class == DIGITS:
        return digits_est(name, kids, pos, sym)
    if function.guard_class == CAPPED:
        return capped_est(function, kids, pos, sym)
    raise refuse(f"'{name}' has no guard class", pos)


def log_est(kids: list, sym: bool) -> Est:
    """`log(a)` is at most ln(10) times a's digits above or below one, if `a` is an exact number."""
    a = kids[0]
    if a.unb or not a.exact:
        return unbounded(sym)
    if len(kids) == 2:
        base = kids[1]
        if base.val is None or base.val[1] != 1 or abs(base.val[0]) < 2:
            return unbounded(sym)
    scale = LN10 * max(max(a.mag, 0.0), a.den, 1.0)
    return Est(math.log10(1 + scale) + SMALL_MAG, 1.0, 1.0, False, None, sym)


def symbolic_est(name: str, kids: list, sym: bool) -> Est:
    first = kids[0]
    if name in KEEP_ARGUMENT:
        val = (abs(first.val[0]), first.val[1]) if name == "Abs" and first.val else None
        return Est(first.mag, first.num, first.den, first.exact, val, sym, first.unb)
    if name == "sign":
        return Est(0.0, 1.0, 1.0, True, None, sym)
    if name in BOUNDED:
        return Est(SMALL_MAG, 1.0, 1.0, False, None, sym)
    if name in POLES:
        return unbounded(sym)
    if name in LOGS:
        return log_est(kids, sym)
    if first.unb:
        return unbounded(sym)
    if name in GROWING:
        bound = 10.0**first.mag if first.mag <= 300 else math.inf
        return Est(EXP_GROWTH * bound + SMALL_MAG, 1.0, 1.0, False, None, sym)
    # asinh and acosh: about ln(2x)
    return Est(
        math.log10(1 + 2.31 * max(first.mag, 0.0)) + 2 * SMALL_MAG, 1.0, 1.0, False, None, sym
    )


def root_est(name: str, kids: list, pos: int, sym: bool) -> Est:
    base = kids[0]
    if name == "sqrt":
        f = 0.5
    elif name == "cbrt":
        f = 1 / 3
    else:
        index = kids[1]
        if index.unb or not index.exact:
            raise refuse("the index of a root must be an exact number", pos)
        if index.val is not None:
            f = 1.0 if index.val[0] == 0 else abs(index.val[1] / index.val[0])
        else:
            # The index may be tiny, and root(x, 1/n) is x^n: bound 1/index by its digits.
            f = 10.0 ** min(index.den, 300)
    return pow_est(base, f, None, pos, sym)


def select_est(name: str, kids: list, sym: bool) -> Est:
    """floor, ceiling, Min, Max, Mod, gcd and lcm.

    Magnitudes are sums or maxima of the inputs' non-negative parts (a signed log can be
    negative: `Max(10^70, 1e-70)` is 10^70, not 10^0).
    """
    unb = any(kid.unb for kid in kids)
    exact = all(kid.exact for kid in kids)
    top = max(kid.mag for kid in kids)
    if name in ("floor", "ceiling"):
        if unb:
            return unbounded(sym)
        mag = max(top, 0.0) + LOG2
        return Est(mag, math.ceil(max(mag, 0.0)) + 1.0, 1.0, True, None, sym)
    if unb:
        return unbounded(sym)
    if name in ("Min", "Max"):
        return Est(top, max(k.num for k in kids), max(k.den for k in kids), exact, None, sym)
    if name == "lcm":
        mag = sum(max(kid.mag, 0.0) for kid in kids)
        return Est(mag, sum(k.num for k in kids), sum(k.den for k in kids), exact, None, sym)
    if name == "gcd":
        return Est(
            max(top, 0.0), max(k.num for k in kids), sum(k.den for k in kids), exact, None, sym
        )
    # Mod(a, b): the result is smaller than |b|
    a, b = kids
    return Est(b.mag, b.num + a.den + 1, a.den + b.den, exact, None, sym)


def lgamma_digits(n: float) -> float:
    """log10 of n! (about), without overflow for absurd arguments."""
    if n > HUGE:
        return math.inf
    return math.lgamma(n + 1) / LN10


def digits_result(digits: float, exact: bool, sym: bool, val: tuple | None = None) -> Est:
    width = math.ceil(max(digits, 0.0)) + 1.0
    return Est(width - 1, width if exact else 1.0, 1.0, exact, val, sym)


def digits_est(name: str, kids: list, pos: int, sym: bool) -> Est:
    first = kids[0]
    n = abs_upper(first)
    exact = all(kid.exact for kid in kids)
    val = None
    result_unbounded = False
    if name in ("factorial", "gamma"):
        v = first.val
        low = v is not None and (v[0] >= 0 if name == "factorial" else v[0] > 0)
        digits = lgamma_digits(n)
        if low:
            if name == "gamma" and v[0] < v[1]:
                # gamma(x) is about 1/x just above zero
                digits = max(digits, math.log10(v[1] / v[0]) + 1)
            if v[1] == 1 and v[0] <= SMALL_FACTORIAL:
                steps = v[0] if name == "factorial" else v[0] - 1
                val = (math.factorial(steps), 1)
        else:
            result_unbounded = True  # a pole, or an argument whose value is not known
    elif name in ("fibonacci", "lucas"):
        digits = FIB_DIGITS_PER_N * n
    else:  # binomial
        second = kids[1]
        n_int, k_int = integer_value(first), integer_value(second)
        k_up = abs_upper(second)
        if k_int is None or k_int < 0:
            digits = 0.0
            result_unbounded = True
        elif n_int is not None and 0 <= k_int <= n_int:
            digits = 0.0  # an argument over 1e6 is refused below
            if n_int <= MAX_EXPONENT:
                digits = (
                    math.lgamma(n_int + 1) - math.lgamma(k_int + 1) - math.lgamma(n_int - k_int + 1)
                ) / LN10
        else:
            # n negative or not a whole number: |C(n, k)| <= C(|n| + k, k) <= 2^(|n| + k)
            if k_int > MAX_GENERALIZED_BINOMIAL_K:
                raise refuse(
                    f"binomial with a negative or fractional n takes k up to "
                    f"{MAX_GENERALIZED_BINOMIAL_K:,}, got {fmt_digits(k_up)}",
                    pos,
                )
            digits = (n + k_int) * LOG2
        if k_up > MAX_EXPONENT:
            digits = max(digits, k_up * LOG2 if k_int is not None else 0.0)
    if digits > MAX_DIGITS:
        raise too_big(digits, pos, f"{name} result")
    if any(abs_upper(kid) > MAX_EXPONENT for kid in kids):
        raise refuse(f"the argument of '{name}' is over 1e6", pos)
    if result_unbounded:
        return unbounded(sym, math.ceil(digits) + 1.0, 1.0)
    return digits_result(digits, exact, sym, val)


def capped_est(function, kids: list, pos: int, sym: bool) -> Est:
    name = function.name
    arg = kids[0]
    if function.cap_kind == "value":
        bound = abs_upper(arg)
        if bound > function.cap:
            raise refuse(
                f"'{name}' takes an argument of at most {function.cap:,}, got about "
                f"{fmt_digits(bound)}",
                pos,
            )
        bound = max(bound, 1.0)
        if name == "prime":
            digits = math.log10(bound) + 1.2
            return Est(digits, math.ceil(digits) + 1.0, 1.0, True, None, sym)
        if name == "harmonic":
            width = math.ceil(bound * EXP_GROWTH) + 1.0
            return Est(math.log(bound + 1), width, width, True, None, sym)
        if name == "bernoulli":
            width = math.ceil(bound * math.log10(bound + 1)) + 1.0
            return Est(width, width, math.ceil(bound * EXP_GROWTH) + 1.0, True, None, sym)
        width = math.ceil(bound * 0.602) + 1.0  # catalan
        return Est(width, width, 1.0, True, None, sym)
    digits = size(arg)
    if digits > function.cap:
        raise refuse(
            f"'{name}' takes an argument of at most {function.cap} digits, got about "
            f"{fmt_digits(digits)}",
            pos,
        )
    if name == "isprime":
        return Est(0.0, 1.0, 1.0, True, None, sym)
    return Est(arg.mag + 0.1, digits + 1, 1.0, True, None, sym)


def visit(node, kids: list, expand: bool):
    if isinstance(node, Num):
        est = num_est(node)
    elif isinstance(node, Const):
        est = constant_est(node)
    elif isinstance(node, Sym):
        return Est(0.0, 1.0, 1.0, True, None, True)
    elif isinstance(node, Rel):
        return Est(0.0, 1.0, 1.0, True, None, any(is_symbolic(kid) for kid in kids))
    elif isinstance(node, Neg):
        kid = kids[0]
        if isinstance(kid, Est) and kid.val is not None:
            return Est(
                kid.mag, kid.num, kid.den, kid.exact, (-kid.val[0], kid.val[1]), kid.sym, kid.unb
            )
        return kid
    elif isinstance(node, BinOp):
        est = binop_est(node, kids[0], kids[1], expand)
    elif isinstance(node, Call):
        est = function_est(node, kids)
    elif isinstance(node, Seq):
        return seq_est(node, kids)
    else:
        raise refuse("unknown syntax node", 0)
    if isinstance(est, Est) and size(est) > MAX_DIGITS:
        raise too_big(size(est), node.pos, "number" if isinstance(node, Num) else "result")
    return est


def check(root, *, expand: bool = False):
    """Check a parsed tree and return its root's estimate (an `Est`, or a `Mat` for a list).

    `expand=True` adds `expand`'s rule: a constant exponent over 10,000 on a base with a symbol
    is refused. Raises `GuardError`.
    """
    values: list = []
    for node in postorder(root):
        count = len(children(node))
        kids = values[len(values) - count :] if count else []
        del values[len(values) - count :]
        values.append(visit(node, kids, expand))
    return values[0]
