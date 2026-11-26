"""The eleven tools of plan D6, as the worker runs them.

A request is `{"tool": <name>, "args": {...}}`; the front checked `args` against the tool's input
schema, and every name is checked again here by the builder. Each tool parses, guards and builds
its expressions through `build.Builder` (with the call's `assume`), calls fixed SymPy functions,
and returns `format.result(...)`. Errors are `CasInputError` (`parse` or `guard`) or `MathError`;
the worker turns any other exception into a `math` reply.

Determinism: nothing here iterates a set or dict in hash order. Variables default to the free
symbols sorted by name, lists (eigenvalues, factors, points) are sorted, and `equal`'s points
come from a `random.Random` with a fixed seed.
"""

import random

from sympy import (
    Abs,
    AccumBounds,
    Complexes,
    ConditionSet,
    Equality,
    Expr,
    FiniteSet,
    I,
    Integer,
    Integral,
    Limit,
    Matrix,
    MatrixBase,
    Rational,
    Reals,
    Rel,
    Tuple,
    Unequality,
    default_sort_key,
    diff,
    expand,
    factor,
    factorint,
    integrate,
    limit,
    linsolve,
    nonlinsolve,
    series,
    simplify,
    solveset,
)

from .build import SYMPY_ERRORS, Builder, MathError, checked_name, refuse
from .format import OUTPUT_CAP, result
from .guard import MAX_TERMS

EQUAL_SEED = 20261126  # `equal`'s random points (D6): fixed, so results are reproducible
EQUAL_POINTS = 10
EQUAL_DIGITS = 30
EQUAL_TOLERANCE = Rational(1, 10**20)
FACTOR_DIGITS = 100  # `factor` of an integer: at most this many digits
LIMIT_DIRECTIONS = {"+": "+", "-": "-", "both": "+-"}  # D6's names to SymPy's `dir`
# The tools that may multiply out powers and products get the guard's expansion estimate
# (`guard.check_expansion`); `differentiate` and `evaluate` do not expand. `expand`, whose
# result is the expanded form, is also refused when that form would be over twice the output
# cap. A heuristic first line; the worker's limits are the backstop.
EXPANDING = frozenset(
    {"simplify", "expand", "factor", "solve", "integrate", "limit", "series", "matrix", "equal"}
)
# Matrix operations whose cost grows like n! or worse on symbolic entries: the largest side
# allowed, for a matrix with symbols and for a numeric one.
MATRIX_SIDES = {
    "det": (8, 30),
    "inverse": (8, 30),
    "charpoly": (8, 30),
    "rref": (8, 30),
    "rank": (8, 30),
    "nullspace": (8, 30),
    "solve": (8, 30),
    "eigenvalues": (4, 10),
    "eigenvectors": (4, 10),
}
MATRIX_OPERATIONS = (
    "value",
    "det",
    "inverse",
    "transpose",
    "rank",
    "rref",
    "nullspace",
    "eigenvalues",
    "eigenvectors",
    "charpoly",
    "solve",
)


# --- Arguments (the front's schema checked them; these checks hold if it was bypassed) ---


def string(args: dict, key: str, required: bool = True) -> str | None:
    value = args.get(key)
    if value is None and not required:
        return None
    if not isinstance(value, str):
        raise refuse(f"{key} must be an expression string")
    return value


def whole(args: dict, key: str, default: int, low: int, high: int) -> int:
    value = args.get(key, default)
    if not isinstance(value, int) or isinstance(value, bool) or not low <= value <= high:
        raise refuse(f"{key} must be a whole number from {low} to {high}")
    return value


def choice(args: dict, key: str, options: tuple, default: str) -> str:
    value = args.get(key, default)
    if value not in options:
        raise refuse(f"{key} must be one of {', '.join(options)}")
    return value


def scalar(builder: Builder, text: str):
    """An expression that must be one value: not a relation, list or matrix."""
    value = builder.expression(text)
    if not isinstance(value, Expr):
        raise MathError("this tool takes one expression, not a relation, list or matrix")
    return value


def by_name(symbols) -> list:
    return sorted(symbols, key=lambda symbol: symbol.name)


def variable(builder: Builder, args: dict, *values):
    """The `variable` argument, or the only free symbol of the values."""
    name = args.get("variable")
    if name is not None:
        return builder.symbol(checked_name(name, "the variable"))
    free = set()
    for value in values:
        free |= value.free_symbols
    if len(free) != 1:
        raise MathError(f"give the variable: the expression has {count_text(len(free))}")
    return free.pop()


def count_text(number: int) -> str:
    return "no symbols" if number == 0 else f"{number} symbols"


def kind_of(value) -> str:
    if isinstance(value, MatrixBase):
        return "matrix"
    if value.has(Integral, Limit):
        return "unevaluated"
    return "expression"


# --- The tools ---


def tool_simplify(builder: Builder, args: dict) -> dict:
    value = builder.expression(string(args, "expression"))
    value = simplify(value)
    return result(value, kind_of(value))


def tool_expand(builder: Builder, args: dict) -> dict:
    value = builder.expression(string(args, "expression"), expand=True)
    value = expand(value)
    return result(value, kind_of(value))


def tool_factor(builder: Builder, args: dict) -> dict:
    value = builder.expression(string(args, "expression"))
    gaussian = args.get("gaussian", False)
    if not isinstance(gaussian, bool):
        raise refuse("gaussian must be true or false")
    if isinstance(value, Integer):
        return factor_integer(int(value))
    return result(factor(value, gaussian=gaussian), "expression")


def factor_integer(number: int) -> dict:
    """`factor("360")` is `2^3*3^2*5`: the prime powers in increasing order."""
    if len(str(abs(number))) > FACTOR_DIGITS:
        raise MathError(f"factor takes integers of at most {FACTOR_DIGITS} digits")
    if abs(number) < 2:
        return result(Integer(number), "expression")
    powers = sorted(factorint(abs(number)).items())
    sign = "-" if number < 0 else ""
    text = sign + "*".join(f"{p}^{k}" if k > 1 else str(p) for p, k in powers)
    shown = sign + r" \cdot ".join(f"{p}^{{{k}}}" if k > 1 else str(p) for p, k in powers)
    return result(
        None, "expression", text=text, latex_text=shown, factors=[[str(p), k] for p, k in powers]
    )


def is_inequality(value) -> bool:
    return isinstance(value, Rel) and not isinstance(value, Equality | Unequality)


def tool_solve(builder: Builder, args: dict) -> dict:
    texts = args.get("equations")
    if not isinstance(texts, list) or not 1 <= len(texts) <= 10:
        raise refuse("equations must be a list of 1 to 10 expression strings")
    domain_name = choice(args, "domain", ("complex", "real"), "complex")
    domain = Reals if domain_name == "real" else Complexes
    if not all(isinstance(text, str) for text in texts):
        raise refuse("each equation must be an expression string")
    values = [builder.expression(text) for text in texts]
    for value in values:
        if isinstance(value, MatrixBase | Tuple):
            raise MathError("an equation must be one expression or relation, not a list")
    names = args.get("variables")
    if names is None:
        free = set()
        for value in values:
            free |= value.free_symbols
        unknowns = by_name(free)
    elif isinstance(names, list) and 1 <= len(names) <= 10:
        unknowns = [builder.symbol(checked_name(name, "the variable")) for name in names]
    else:
        raise refuse("variables must be a list of 1 to 10 names")
    if not unknowns:
        raise MathError("there is nothing to solve for: the equations have no symbols")
    if any(is_inequality(value) for value in values):
        if domain_name == "complex":
            raise MathError("inequalities are solved over the reals only: pass domain real")
        if len(values) > 1 or len(unknowns) > 1:
            raise MathError("solve one inequality in one variable at a time")
    if len(values) == 1 and len(unknowns) == 1:
        found = solveset(values[0], unknowns[0], domain)
        kind = "set"
    else:
        found = solve_system(values, unknowns, domain_name)
        kind = "solutions"
    if found.has(ConditionSet):
        return result(found, "unevaluated", domain=domain_name, note="not solved")
    return result(found, kind, domain=domain_name)


def solve_system(values: list, unknowns: list, domain_name: str):
    """`linsolve` for a linear system, else `nonlinsolve`; over the reals, solutions with a
    component that is certainly not real are dropped."""
    zeros = [v.lhs - v.rhs if isinstance(v, Equality) else v for v in values]
    if any(isinstance(v, Rel) for v in zeros):
        raise MathError("a system takes equations only, not inequalities")
    try:
        found = linsolve(zeros, *unknowns)
    except ValueError:  # NonlinearError
        found = nonlinsolve(zeros, *unknowns)
    if domain_name == "real" and isinstance(found, FiniteSet):
        kept = [s for s in found if not any(part.is_real is False for part in s)]
        found = FiniteSet(*kept)
    return found


def tool_differentiate(builder: Builder, args: dict) -> dict:
    value = scalar(builder, string(args, "expression"))
    order = whole(args, "order", 1, 1, 20)
    found = diff(value, variable(builder, args, value), order)
    return result(found, kind_of(found))


def tool_integrate(builder: Builder, args: dict) -> dict:
    value = scalar(builder, string(args, "expression"))
    lower, upper = string(args, "lower", False), string(args, "upper", False)
    if (lower is None) != (upper is None):
        raise refuse("give both lower and upper, or neither")
    var = variable(builder, args, value)
    if lower is None:
        found = integrate(value, var)
        if found.has(Integral):
            return result(found, "unevaluated", note="no antiderivative found")
        return result(found, kind_of(found), note="antiderivative, without + C")
    bounds = (var, builder.expression(lower), builder.expression(upper))
    found = integrate(value, bounds)
    if not found.has(Integral):
        return result(found, kind_of(found))
    numeric = Integral(value, bounds).evalf(15)
    note = "no closed form found"
    if numeric.is_number and not numeric.has(Integral):
        note += f"; numerically {numeric} (15 digits)"
    return result(found, "unevaluated", note=note)


def tool_limit(builder: Builder, args: dict) -> dict:
    value = scalar(builder, string(args, "expression"))
    var = variable(builder, args, value)
    to = builder.expression(string(args, "to"))
    direction = choice(args, "direction", ("+", "-", "both"), "both")
    try:
        found = limit(value, var, to, LIMIT_DIRECTIONS[direction])
    except ValueError as exc:
        if not str(exc).startswith("The limit does not exist"):
            raise
        return result(
            None,
            "unevaluated",
            text="does not exist",
            latex_text=r"\text{does not exist}",
            exists=False,
            note="the one-sided limits differ",
        )
    if isinstance(found, AccumBounds):
        note = f"it stays between {found.min} and {found.max}"
        return result(
            None,
            "unevaluated",
            text="does not exist",
            latex_text=r"\text{does not exist}",
            exists=False,
            note=note,
        )
    return result(found, kind_of(found))


def tool_series(builder: Builder, args: dict) -> dict:
    value = scalar(builder, string(args, "expression"))
    var = variable(builder, args, value)
    at = builder.expression(string({"at": args.get("at", "0")}, "at"))
    order = whole(args, "order", 6, 1, 50)
    return result(series(value, var, at, n=order), "expression")


def column(value) -> MatrixBase:
    if isinstance(value, Tuple):
        return Matrix(list(value))
    if isinstance(value, MatrixBase) and value.cols == 1:
        return value
    if isinstance(value, MatrixBase) and value.rows == 1:
        return value.T
    raise MathError("rhs must be a list or a column, such as [1, 2]")


def tool_matrix(builder: Builder, args: dict) -> dict:
    value = builder.expression(string(args, "matrix"))
    if not isinstance(value, MatrixBase):
        raise MathError("matrix needs a matrix value, such as [[1, 2], [3, 4]]")
    operation = choice(args, "operation", MATRIX_OPERATIONS, "value")
    if operation in MATRIX_SIDES:
        symbolic, numeric = MATRIX_SIDES[operation]
        largest = symbolic if value.free_symbols else numeric
        if max(value.shape) > largest:
            kind = "with symbols" if value.free_symbols else "of numbers"
            raise MathError(
                f"{operation} of a matrix {kind} takes at most {largest} rows and columns"
                " (its cost grows too fast beyond that)"
            )
    if operation == "value":
        return result(value, "matrix")
    if operation == "det":
        return result(value.det(), "expression")
    if operation == "inverse":
        return result(value.inv(), "matrix")
    if operation == "transpose":
        return result(value.T, "matrix")
    if operation == "rank":
        return result(Integer(value.rank()), "expression")
    if operation == "rref":
        reduced, pivots = value.rref()
        return result(reduced, "matrix", pivots=list(pivots))
    if operation == "nullspace":
        return result([list(vector) for vector in value.nullspace()], "list")
    if operation == "eigenvalues":
        found = sorted(value.eigenvals().items(), key=lambda item: default_sort_key(item[0]))
        listed = [eigen for eigen, count in found for _ in range(count)]
        return result(listed, "list", note="each eigenvalue as often as its multiplicity")
    if operation == "eigenvectors":
        found = sorted(value.eigenvects(), key=lambda item: default_sort_key(item[0]))
        listed = [
            [eigen, Integer(count), [list(v) for v in vectors]] for eigen, count, vectors in found
        ]
        return result(listed, "list", note="each entry is [eigenvalue, multiplicity, vectors]")
    if operation == "charpoly":
        return result(value.charpoly(builder.symbol("lambda")).as_expr(), "expression")
    rhs = column(builder.expression(string(args, "rhs")))
    return result(linsolve((value, rhs)), "solutions")


def tool_evaluate(builder: Builder, args: dict) -> dict:
    value = builder.expression(string(args, "expression"))
    digits = whole(args, "digits", 15, 1, 1000)
    at = builder.at(args.get("at", {}))
    found = value.evalf(n=digits, subs=at)
    if isinstance(found, MatrixBase):
        return result(found, "matrix", digits=digits)
    return result(found, "number" if found.is_number else "expression", digits=digits)


def sample(rng: random.Random, symbol):
    """A random exact point for `symbol` that satisfies its assumptions."""
    if symbol.is_integer:
        value = Integer(rng.randint(1, 9))
    else:
        value = Rational(rng.randint(1, 900), 100)
    if symbol.is_negative or symbol.is_nonpositive:
        value = -value
    elif not (symbol.is_positive or symbol.is_nonnegative) and rng.random() < 0.5:
        value = -value
    if symbol.is_real is None and symbol.is_integer is None:
        value += Rational(rng.randint(-900, 900), 100) * I
    return value


def tool_equal(builder: Builder, args: dict) -> dict:
    left = builder.expression(string(args, "left"))
    right = builder.expression(string(args, "right"))
    if isinstance(left, MatrixBase) or isinstance(right, MatrixBase):
        if not (isinstance(left, MatrixBase) and isinstance(right, MatrixBase)):
            raise MathError("compare a matrix with a matrix of the same shape")
        if left.shape != right.shape:
            raise MathError(f"the shapes differ: {left.shape} and {right.shape}")
        differences = list(left - right)
    else:
        differences = [left - right]
    if all(simplify(d) == 0 for d in differences) or all(d.equals(0) is True for d in differences):
        return verdict("true", "symbolic", "proved equal")
    free = set()
    for d in differences:
        free |= d.free_symbols
    symbols = by_name(free)
    rng = random.Random(EQUAL_SEED)  # noqa: S311 - reproducible test points, not secrets
    for _ in range(EQUAL_POINTS):
        point = {symbol: sample(rng, symbol) for symbol in symbols}
        for d in differences:
            gap = Abs(d).evalf(EQUAL_DIGITS, subs=point)
            if gap.is_number and gap.is_finite and gap > EQUAL_TOLERANCE:
                shown = {symbol.name: str(value) for symbol, value in point.items()}
                where = ", ".join(f"{name} = {value}" for name, value in shown.items())
                note = f"differs at {where}" if where else "the values differ"
                return verdict("false", "numeric", note, point=shown, difference=str(gap.evalf(6)))
    return verdict("unknown", "numeric", f"agrees at {EQUAL_POINTS} random points, not proved")


def verdict(answer: str, method: str, note: str, **extra) -> dict:
    return result(
        None,
        "boolean",
        text=answer,
        latex_text=r"\text{" + answer + "}",
        method=method,
        note=note,
        **extra,
    )


TOOLS = {
    "simplify": tool_simplify,
    "expand": tool_expand,
    "factor": tool_factor,
    "solve": tool_solve,
    "differentiate": tool_differentiate,
    "integrate": tool_integrate,
    "limit": tool_limit,
    "series": tool_series,
    "matrix": tool_matrix,
    "evaluate": tool_evaluate,
    "equal": tool_equal,
}


def run_tool(name: str, args) -> dict:
    """Run one tool. Raises `CasInputError` or `MathError` for the worker to report."""
    if not isinstance(args, dict):
        raise refuse("the arguments must be an object")
    try:
        builder = Builder(args.get("assume"))
        if name in EXPANDING:
            builder.max_terms = MAX_TERMS
            builder.max_chars = 2 * OUTPUT_CAP if name == "expand" else None
        return TOOLS[name](builder, args)
    except SYMPY_ERRORS as exc:
        raise MathError(str(exc)[:500] or "SymPy refused the input") from None
    except RecursionError:
        raise MathError("the expression is nested too deeply for SymPy") from None


# Every tool's code paths, run by the worker's warm-up before the audit hook, so nothing is
# imported lazily during a call (a missed import fails closed with `refused`).
WARM_UP = (
    ("simplify", {"expression": "sin(x)^2 + cos(x)^2 + (x^2 - 1)/(x - 1)"}),
    ("simplify", {"expression": "[[x, 2x], [1, x^2]]", "assume": {"x": ["positive"]}}),
    ("expand", {"expression": "(x + y)^3 * (x - 1)"}),
    ("factor", {"expression": "360"}),
    ("factor", {"expression": "x^4 - 1", "gaussian": True}),
    ("factor", {"expression": "x^2 - 2x + 1"}),
    ("solve", {"equations": ["x^2 = 4"]}),
    ("solve", {"equations": ["x^2 - 4 < 0"], "domain": "real"}),
    ("solve", {"equations": ["sin(x) = 0"], "domain": "real"}),
    ("solve", {"equations": ["exp(x) + x = 0"], "domain": "real"}),
    ("solve", {"equations": ["x^3 + 2x - 5 = 0"]}),
    ("solve", {"equations": ["x + y = 2", "x - y = 0"]}),
    ("solve", {"equations": ["x^2 + y^2 = 1", "x = y"], "domain": "real"}),
    ("solve", {"equations": ["x < 1"]}),
    ("differentiate", {"expression": "sin(x) * exp(x^2) * log(x)", "order": 2}),
    ("integrate", {"expression": "x * exp(x) + 1/(x^2 + 1) + sin(x)^2"}),
    ("integrate", {"expression": "exp(-x^2)", "variable": "x", "lower": "0", "upper": "oo"}),
    ("integrate", {"expression": "exp(x^x)", "variable": "x", "lower": "0", "upper": "1"}),
    ("integrate", {"expression": "exp(x^x)", "variable": "x"}),
    ("limit", {"expression": "sin(x)/x", "variable": "x", "to": "0"}),
    ("limit", {"expression": "Abs(x)/x", "variable": "x", "to": "0"}),
    ("limit", {"expression": "sin(1/x)", "variable": "x", "to": "0", "direction": "+"}),
    ("limit", {"expression": "(1 + 1/x)^x", "variable": "x", "to": "oo"}),
    ("series", {"expression": "exp(x) * cos(x)", "variable": "x", "order": 5}),
    ("series", {"expression": "1/(1 - x)", "variable": "x", "at": "oo", "order": 3}),
    *(
        ("matrix", {"matrix": "[[2, 1], [1, 3]]", "operation": operation, "rhs": "[1, 2]"})
        for operation in MATRIX_OPERATIONS
    ),
    ("matrix", {"matrix": "[[1, 2], [2, 4]]", "operation": "inverse"}),
    ("evaluate", {"expression": "sqrt(2) + pi*x + I", "digits": 30, "at": {"x": "1/3"}}),
    ("evaluate", {"expression": "[[pi, E]]", "digits": 5}),
    ("equal", {"left": "sin(x)^2", "right": "1 - cos(x)^2"}),
    ("equal", {"left": "sqrt(x^2)", "right": "x"}),
    ("equal", {"left": "x + 1", "right": "x", "assume": {"x": ["integer", "positive"]}}),
    ("equal", {"left": "[[x, 1]]", "right": "[[x, 2]]"}),
)
