"""The magnitude guard (plan D2): a bottom-up size estimate over the parsed tree.

For every subtree whose leaves are all constants, the guard computes an upper bound on the size of
the result and never computes the value. The estimate is in base-10 logarithms and digit counts:

- `mag`: log10 of the absolute value (an upper bound);
- `num` and `den`: digits of an exact rational's numerator and denominator, or infinity when the
  value is not an exact rational;
- `val`: the exact value as a `(p, q)` pair, kept only while it is small (at most 64 digits),
  so a small exponent such as `10^6` is known exactly. Nothing big is ever built.

A node whose estimate is over 100,000 digits is a `GuardError` that names the estimate. Subtrees
with symbols have no estimate (`None`); the guard still checks their constant parts and the
structural rules (an exponent that is a huge constant, a matrix too big, a function argument over
its cap). What the guard cannot see is stopped by the worker's resource limits (plan D3).

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
MAX_EXPONENT_MAG = 6  # an exponent above 10**6 is refused unless the base is 0, 1 or -1
MAX_MATRIX_SIDE = 30
MAX_MATRIX_ENTRIES = 400
MAX_SYMBOLIC_MATRIX_POWER = 1000
MAX_EXPAND_EXPONENT = 10_000
VALUE_DIGITS = 64  # exact values are tracked while they have at most this many digits

LOG2 = math.log10(2)
LN10 = math.log(10)
FIB_DIGITS_PER_N = 0.209  # digits of fibonacci(n) and lucas(n) per unit of n, rounded up
EXP_GROWTH = 1 / LN10  # log10(e): exp(x) has about x * 0.4343 digits
HUGE = 1e300

# Known constants: log10 of their value. They are not exact rationals.
CONSTANT_MAG = {"pi": math.log10(math.pi), "E": math.log10(math.e), "I": 0.0, "oo": 0.0}

GROWING = frozenset({"exp", "sinh", "cosh"})
KEEP_ARGUMENT = frozenset({"Abs", "re", "im", "conjugate"})


class GuardError(CasInputError):
    """The guard refused an input that would be too large. `digits` is the estimate, if any."""

    category = "guard"

    @property
    def digits(self) -> float | None:
        return self.args[2] if len(self.args) > 2 else None


@dataclass(frozen=True, slots=True)
class Est:
    """The estimate for a constant scalar subtree."""

    mag: float
    num: float
    den: float
    exact: bool
    val: tuple | None = None


@dataclass(frozen=True, slots=True)
class Mat:
    """The estimate for a list or matrix: its shape and the digits of its largest entry."""

    rows: int
    cols: int
    digits: float
    symbolic: bool
    matrix: bool  # two-dimensional (a list of rows)


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
    if est.exact:
        return max(est.num, est.den)
    return max(est.mag, 0.0) + 1


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
    mag = max(a.mag, b.mag) + LOG2
    if not (a.exact and b.exact):
        return Est(mag, math.inf, math.inf, False)
    val = None
    if a.val is not None and b.val is not None:
        (p1, q1), (p2, q2) = a.val, b.val
        val = keep_val(p1 * q2 + (-p2 if sub else p2) * q1, q1 * q2)
    return Est(mag, max(a.num + b.den, b.num + a.den) + 1, a.den + b.den, True, val)


def mul_est(a: Est, b: Est) -> Est:
    if not (a.exact and b.exact):
        return Est(a.mag + b.mag, math.inf, math.inf, False)
    val = None
    if a.val is not None and b.val is not None:
        val = keep_val(a.val[0] * b.val[0], a.val[1] * b.val[1])
    return Est(a.mag + b.mag, a.num + b.num, a.den + b.den, True, val)


def div_est(a: Est, b: Est) -> Est:
    b_mag = 0.0 if b.val is not None and b.val[0] == 0 else b.mag
    mag = a.mag - b_mag
    if not (a.exact and b.exact):
        return Est(mag, math.inf, math.inf, False)
    val = None
    if a.val is not None and b.val is not None:
        val = keep_val(a.val[0] * b.val[1], a.val[1] * b.val[0])
    return Est(mag, a.num + b.den, a.den + b.num, True, val)


def integer_value(est: Est) -> int | None:
    """The exact integer value of a constant, when it is known and an integer."""
    if est.val is not None and est.val[1] == 1:
        return est.val[0]
    return None


def abs_upper(est: Est) -> float:
    """An upper bound on the absolute value of a constant (a float, `inf` when huge)."""
    if est.val is not None:
        return abs(est.val[0]) / est.val[1]
    if est.mag > 300:
        return math.inf
    return 10.0**est.mag


def pow_est(base: Est, f: float, exp_int: int | None, exp_mag: float, pos: int) -> Est:
    """`base` raised to an exponent whose absolute value is at most `f`.

    `exp_int` is the exponent when it is a known integer; `exp_mag` its log10.
    """
    if base.val is not None and base.val[0] in (0, 1, -1) and base.val[1] == 1:
        # 0, 1 and -1 stay 0, 1 and -1 (or 0 and 1) whatever the exponent: nothing grows.
        keep = base.val[0] == 1 or (base.val[0] == 0 and exp_int is not None and exp_int > 0)
        return Est(0.0, 1.0, 1.0, True, base.val if keep else None)
    whole = base.exact and base.den == 1
    if whole:
        # A whole number's own log10 is tighter than its digit count: 10^199 has 200 digits.
        estimate = f * max(base.mag, 0.0) + 1
    elif base.exact:
        estimate = f * max(base.num, base.den)
    else:
        estimate = abs(base.mag) * f + 1
    if exp_mag > MAX_EXPONENT_MAG:
        raise refuse(
            f"the exponent is too large (about {fmt_digits(f)}): the result would have "
            f"about {fmt_digits(estimate)} digits (limit {MAX_DIGITS:,})",
            pos,
            estimate,
        )
    if estimate > MAX_DIGITS:
        raise too_big(estimate, pos)
    mag = abs(base.mag) * f
    if base.exact and exp_int is not None:
        n = abs(exp_int)
        if whole:
            big = math.ceil(n * max(base.mag, 0.0)) + 1.0
            num, den = (big, 1.0) if exp_int >= 0 else (1.0, big)
        elif exp_int >= 0:
            num, den = base.num * n, base.den * n
        else:
            num, den = base.den * n, base.num * n
        val = None
        if base.val is not None and n <= 1000 and mag + 1 <= VALUE_DIGITS:
            p, q = base.val
            if exp_int >= 0:
                val = keep_val(p**n, q**n)
            elif p != 0:
                val = keep_val(q**n, p**n)
        return Est(mag, num, den, True, val)
    return Est(mag, math.inf, math.inf, False)


def constant_est(node: Const) -> Est:
    return Est(CONSTANT_MAG[node.name], math.inf, math.inf, False)


def entry_digits(item: Est | Mat | None) -> float:
    """Digits of a scalar or of a matrix's largest entry (1 for a symbol)."""
    if isinstance(item, Est):
        return size(item)
    if isinstance(item, Mat):
        return item.digits
    return 1.0


def is_symbolic(item: Est | Mat | None) -> bool:
    if isinstance(item, Mat):
        return item.symbolic
    return item is None


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
    return Mat(1, len(kids), digits, any(kid is None for kid in kids), False)


def matrix_power(base: Mat, exponent, pos: int) -> Mat:
    n = integer_value(exponent) if isinstance(exponent, Est) else None
    if n is None:
        raise refuse("a matrix power needs a constant integer exponent", pos)
    n = abs(n)
    if base.symbolic and n > MAX_SYMBOLIC_MATRIX_POWER:
        raise refuse(
            f"a symbolic matrix power is limited to exponents up to {MAX_SYMBOLIC_MATRIX_POWER}",
            pos,
        )
    digits = n * (base.digits + math.log10(max(base.rows, base.cols, 1)))
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
    if left is None or right is None:
        return None
    if op == "+":
        return add_est(left, right, False)
    if op == "-":
        return add_est(left, right, True)
    if op == "*":
        return mul_est(left, right)
    return div_est(left, right)


def power_node(base, exponent, pos: int, expand: bool):
    if exponent is None:
        return None  # a symbolic exponent: nothing to size
    if base is None:
        if exponent.mag > MAX_EXPONENT_MAG:
            raise refuse(
                f"the exponent is too large (about {fmt_digits(abs_upper(exponent))}; limit 1e6)",
                pos,
            )
        if expand:
            if exponent.val is None:
                big = exponent.mag > math.log10(MAX_EXPAND_EXPONENT)
            else:
                big = abs(exponent.val[0]) > MAX_EXPAND_EXPONENT * exponent.val[1]
            if big:
                raise refuse(
                    f"expand refuses an exponent over {MAX_EXPAND_EXPONENT:,} on a symbolic base",
                    pos,
                )
        return None
    return pow_est(base, abs_upper(exponent), integer_value(exponent), exponent.mag, pos)


def function_est(node: Call, kids: list):
    name, pos = node.name, node.pos
    function = FUNCTIONS[name]
    for kid in kids:
        if isinstance(kid, Mat):
            raise refuse(f"'{name}' does not take a list or matrix", pos)
    if any(kid is None for kid in kids):
        return None
    if function.guard_class == SYMBOLIC:
        return symbolic_est(name, kids)
    if function.guard_class == ROOT:
        return root_est(name, kids, pos)
    if function.guard_class == SELECT:
        return select_est(kids)
    if function.guard_class == DIGITS:
        return digits_est(name, kids, pos)
    if function.guard_class == CAPPED:
        return capped_est(function, kids, pos)
    raise refuse(f"'{name}' has no guard class", pos)


def symbolic_est(name: str, kids: list) -> Est:
    if name in KEEP_ARGUMENT:
        first = kids[0]
        val = (abs(first.val[0]), first.val[1]) if name == "Abs" and first.val else None
        return Est(first.mag, first.num, first.den, first.exact, val)
    if name == "sign":
        return Est(0.0, 1.0, 1.0, True)
    mag = max(kid.mag for kid in kids)
    if name in GROWING:
        mag = EXP_GROWTH * 10.0**mag if mag <= 300 else math.inf
    return Est(mag, math.inf, math.inf, False)


def root_est(name: str, kids: list, pos: int) -> Est:
    base = kids[0]
    if name == "sqrt":
        return pow_est(base, 0.5, None, -0.3, pos)
    if name == "cbrt":
        return pow_est(base, 1 / 3, None, -0.5, pos)
    index = kids[1]
    f = 1.0
    if index.val is not None and index.val[0] != 0:
        f = abs(index.val[1] / index.val[0])
    return pow_est(base, f, None, -0.3, pos)


def select_est(kids: list) -> Est:
    est = kids[0]
    for kid in kids[1:]:
        est = mul_est(est, kid)
    return Est(est.mag, est.num, est.den, est.exact)


def lgamma_digits(n: float) -> float:
    """log10 of n! (about), without overflow for absurd arguments."""
    if n > HUGE:
        return math.inf
    return math.lgamma(n + 1) / LN10


def digits_est(name: str, kids: list, pos: int) -> Est:
    n = abs_upper(kids[0])
    if name in ("factorial", "gamma"):
        digits = lgamma_digits(n)
    elif name in ("fibonacci", "lucas"):
        digits = FIB_DIGITS_PER_N * n
    else:  # binomial
        n_int, k_int = integer_value(kids[0]), integer_value(kids[1])
        if n_int is not None and k_int is not None and 0 <= k_int <= n_int:
            digits = (
                math.lgamma(n_int + 1) - math.lgamma(k_int + 1) - math.lgamma(n_int - k_int + 1)
            ) / LN10
        else:
            digits = n * LOG2
    digits = max(digits, 0.0)
    if digits > MAX_DIGITS:
        raise too_big(digits, pos, f"{name} result")
    if any(kid.mag > MAX_EXPONENT_MAG for kid in kids):
        raise refuse(f"the argument of '{name}' is over 1e6", pos)
    width = math.ceil(digits) + 1.0
    if all(kid.exact for kid in kids):
        return Est(width - 1, width, 1.0, True)
    return Est(width - 1, math.inf, math.inf, False)


def capped_est(function, kids: list, pos: int) -> Est:
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
            return Est(digits, math.ceil(digits) + 1.0, 1.0, True)
        if name == "harmonic":
            width = math.ceil(bound * EXP_GROWTH) + 1.0
            return Est(math.log(bound + 1), width, width, True)
        if name == "bernoulli":
            width = math.ceil(bound * math.log10(bound + 1)) + 1.0
            return Est(width, width, math.ceil(bound * EXP_GROWTH) + 1.0, True)
        width = math.ceil(bound * 0.602) + 1.0  # catalan
        return Est(width, width, 1.0, True)
    digits = size(arg)
    if digits > function.cap:
        raise refuse(
            f"'{name}' takes an argument of at most {function.cap} digits, got about "
            f"{fmt_digits(digits)}",
            pos,
        )
    if name == "isprime":
        return Est(0.0, 1.0, 1.0, True)
    return Est(arg.mag + 0.1, digits + 1, 1.0, True)


def visit(node, kids: list, expand: bool):
    if isinstance(node, Num):
        est = num_est(node)
    elif isinstance(node, Const):
        est = constant_est(node)
    elif isinstance(node, Sym | Rel):
        return None
    elif isinstance(node, Neg):
        kid = kids[0]
        if isinstance(kid, Est) and kid.val is not None:
            return Est(kid.mag, kid.num, kid.den, kid.exact, (-kid.val[0], kid.val[1]))
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
    """Check a parsed tree and return its root's estimate (`None` for a symbolic result).

    `expand=True` adds `expand`'s rule: a constant integer exponent over 10,000 on a
    non-numeric base is refused. Raises `GuardError`.
    """
    values: list = []
    for node in postorder(root):
        count = len(children(node))
        kids = values[len(values) - count :] if count else []
        del values[len(values) - count :]
        values.append(visit(node, kids, expand))
    return values[0]
