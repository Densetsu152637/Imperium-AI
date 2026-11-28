"""The table builder (plan D1): a guarded syntax tree into SymPy objects.

Nothing here parses text into code. A tree from `parse.py` that passed `guard.py` is built bottom
up through fixed tables:

- numbers reach SymPy as Python `int`s (`Integer(m * 10**e)`, or `Rational(m, 10**-e)`);
- `pi`, `E`, `I` and `oo` are SymPy's constants;
- a call resolves its name in `BUILDERS`, a frozen mapping to explicitly imported callables; a
  name that is not there is an error, never an attribute lookup;
- a symbol is `Symbol(name, **assumptions)`, and only after its name passed the identifier check
  a second time (the input schema is the first). `Symbol` does not parse its name;
- operators are Python's operators on SymPy objects, and relations are `Eq`, `Ne`, `Lt`, `Le`,
  `Gt` and `Ge`.

So the only strings that reach SymPy are validated symbol names, and every other argument is
already a SymPy object. The walk is iterative (`parse.postorder`): a valid tree can be 2,000
levels deep (`1+1+...`) or 4,000 (`x!!!...`).
"""

from dataclasses import dataclass
from types import MappingProxyType

from sympy import (
    Abs,
    E,
    Eq,
    Ge,
    Gt,
    I,
    Integer,
    Le,
    Lt,
    Matrix,
    Max,
    Min,
    Mod,
    Ne,
    Rational,
    Symbol,
    Tuple,
    acos,
    acosh,
    acot,
    arg,
    asin,
    asinh,
    atan,
    atan2,
    atanh,
    bernoulli,
    binomial,
    catalan,
    cbrt,
    ceiling,
    conjugate,
    cos,
    cosh,
    cot,
    coth,
    csc,
    erf,
    exp,
    factorial,
    false,
    fibonacci,
    floor,
    gamma,
    gcd,
    harmonic,
    im,
    isprime,
    lcm,
    log,
    lucas,
    nextprime,
    oo,
    pi,
    prevprime,
    prime,
    re,
    root,
    sec,
    sign,
    sin,
    sinh,
    sqrt,
    sstr,
    tan,
    tanh,
    true,
)

from .functions import FUNCTIONS
from .guard import check, check_expansion
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
    is_identifier,
    parse,
    postorder,
)

# The assumptions `assume` may give a symbol (plan D6).
ASSUMPTIONS = frozenset(
    {
        "real",
        "positive",
        "negative",
        "nonnegative",
        "nonpositive",
        "nonzero",
        "integer",
        "rational",
        "finite",
    }
)

# The errors SymPy raises for mathematically invalid input (a shape mismatch, `I < 1`, a symbol
# where an integer is needed). Anything else is a bug or a resource problem and is not caught.
SYMPY_ERRORS = (ValueError, TypeError, ArithmeticError, NotImplementedError)


class BuildError(CasInputError):
    """A name or parameter the builder refused: the input schema's checks, made again."""

    category = "parse"


class MathError(Exception):
    """SymPy refused to build or print the input."""

    category = "math"

    @property
    def message(self) -> str:
        return self.args[0]


def refuse(message: str) -> BuildError:
    return BuildError(message, 1)


def fold(function, args: list):
    """Apply a two-argument function across `args`: SymPy's `gcd(a, b, c)` reads `c` as a
    generator, so the variadic `gcd` and `lcm` fold pairwise."""
    value = args[0]
    for item in args[1:]:
        value = function(value, item)
    return value


def integer_result(function):
    """SymPy's prime functions return a Python `int` or `bool`: make it a SymPy object."""

    def call(*args):
        value = function(*args)
        if value is True:
            return true
        if value is False:
            return false
        return Integer(value)

    return call


_BUILDERS = {
    "sin": sin,
    "cos": cos,
    "tan": tan,
    "cot": cot,
    "sec": sec,
    "csc": csc,
    "asin": asin,
    "acos": acos,
    "atan": atan,
    "acot": acot,
    "atan2": atan2,
    "sinh": sinh,
    "cosh": cosh,
    "tanh": tanh,
    "coth": coth,
    "asinh": asinh,
    "acosh": acosh,
    "atanh": atanh,
    "exp": exp,
    "log": log,
    "erf": erf,
    "Abs": Abs,
    "sign": sign,
    "re": re,
    "im": im,
    "arg": arg,
    "conjugate": conjugate,
    "sqrt": sqrt,
    "cbrt": cbrt,
    "root": root,
    "floor": floor,
    "ceiling": ceiling,
    "Min": Min,
    "Max": Max,
    "Mod": Mod,
    "factorial": factorial,
    "gamma": gamma,
    "binomial": binomial,
    "fibonacci": fibonacci,
    "lucas": lucas,
    "harmonic": harmonic,
    "bernoulli": bernoulli,
    "catalan": catalan,
    "ln": log,
    "gcd": lambda *args: fold(gcd, list(args)),
    "lcm": lambda *args: fold(lcm, list(args)),
    "prime": integer_result(prime),
    "nextprime": integer_result(nextprime),
    "prevprime": integer_result(prevprime),
    "isprime": integer_result(isprime),
}

# The frozen name -> callable table. It has exactly the names of `functions.FUNCTIONS`.
BUILDERS = MappingProxyType(_BUILDERS)

CONSTANT_VALUES = MappingProxyType({"pi": pi, "E": E, "I": I, "oo": oo})
RELATION_BUILDERS = MappingProxyType({"==": Eq, "!=": Ne, "<": Lt, "<=": Le, ">": Gt, ">=": Ge})


def binop(op: str, left, right):
    if op == "+":
        return left + right
    if op == "-":
        return left - right
    if op == "*":
        return left * right
    if op == "/":
        return left / right
    if op == "^":
        return left**right
    raise refuse(f"unknown operator {op[:8]!r}")


def number(node: Num):
    mantissa, exponent = int(node.mantissa), int(node.exponent)
    if mantissa == 0:
        return Integer(0)  # `0e999999999` is 0; computing `10**999999999` would never finish
    if exponent >= 0:
        return Integer(mantissa * 10**exponent)
    return Rational(mantissa, 10**-exponent)


def assumption_flags(name: str, given) -> dict[str, bool]:
    if not isinstance(given, list | tuple):
        raise refuse(f"the assumptions of {name} must be a list")
    for item in given:
        if not isinstance(item, str) or item not in ASSUMPTIONS:
            shown = item[:40] if isinstance(item, str) else "a non-string"
            raise refuse(
                f"unknown assumption {shown!r} for {name}; known: {', '.join(sorted(ASSUMPTIONS))}"
            )
    # The keys are the table's own strings, not the caller's.
    return {flag: True for flag in sorted(ASSUMPTIONS) if flag in given}


def checked_name(name, what: str) -> str:
    """The second identifier check (the input schema's `pattern` is the first)."""
    if not isinstance(name, str) or not is_identifier(name):
        shown = repr(name[:40]) if isinstance(name, str) else "a non-string"
        raise refuse(f"{what} {shown} is not a valid name (letters, digits and single _)")
    return name


@dataclass
class Builder:
    """Builds trees for one call. Symbols with the same name are the same `Symbol`."""

    assume: dict | None = None
    # The expansion estimate's limits for this call's tool (`guard.check_expansion`), or `None`
    # for a tool that does not expand: the most terms, and for `expand` the most characters.
    max_terms: float | None = None
    max_chars: float | None = None

    def __post_init__(self) -> None:
        given = {} if self.assume is None else self.assume
        if not isinstance(given, dict):
            raise refuse("assume must be an object from names to lists of assumptions")
        self.flags: dict[str, dict[str, bool]] = {}
        for name, items in given.items():
            self.flags[checked_name(name, "the assume key")] = assumption_flags(name, items)
        self.symbols: dict[str, Symbol] = {}
        for name in self.flags:
            self.symbol(name)  # a contradictory list is an error even if the name is unused

    def symbol(self, name) -> Symbol:
        """The symbol for a `variable`, `variables` entry, `at` key or `Sym` node."""
        name = checked_name(name, "the name")
        found = self.symbols.get(name)
        if found is None:
            try:
                found = Symbol(name, **self.flags.get(name, {}))
            except ValueError as exc:  # InconsistentAssumptions
                raise MathError(f"the assumptions of {name} contradict each other: {exc}") from None
            self.symbols[name] = found
        return found

    def build(self, root):
        """Build a guarded tree. Raises `BuildError` or `MathError`."""
        values: list = []
        try:
            for node in postorder(root):
                count = len(children(node))
                kids = values[len(values) - count :] if count else []
                del values[len(values) - count :]
                values.append(self.node(node, kids))
        except SYMPY_ERRORS as exc:
            raise MathError(str(exc)[:500] or "SymPy refused the input") from None
        except RecursionError:
            raise MathError("the expression is nested too deeply for SymPy") from None
        return values[0]

    def node(self, node, kids: list):
        if isinstance(node, Num):
            return number(node)
        if isinstance(node, Sym):
            return self.symbol(node.name)
        if isinstance(node, Const):
            value = CONSTANT_VALUES.get(node.name)
            if value is None:
                raise refuse(f"unknown constant {node.name[:40]!r}")
            return value
        if isinstance(node, Neg):
            return -kids[0]
        if isinstance(node, BinOp):
            return binop(node.op, kids[0], kids[1])
        if isinstance(node, Rel):
            relation = RELATION_BUILDERS.get(node.op)
            if relation is None:
                raise refuse(f"unknown relation {node.op[:8]!r}")
            return relation(kids[0], kids[1])
        if isinstance(node, Call):
            return self.call(node, kids)
        if isinstance(node, Seq):
            if all(isinstance(item, Seq) for item in node.items):
                return Matrix([list(row) for row in kids])
            return Tuple(*kids)
        raise refuse("unknown syntax node")

    def call(self, node: Call, kids: list):
        function, builder = FUNCTIONS.get(node.name), BUILDERS.get(node.name)
        if function is None or builder is None:
            raise refuse(f"unknown function {node.name[:40]!r}")
        if len(kids) < function.min_args or (
            function.max_args is not None and len(kids) > function.max_args
        ):
            raise refuse(f"{node.name} takes a different number of arguments")
        if any(isinstance(kid, Matrix | Tuple) for kid in kids):
            raise refuse(f"{node.name} takes values, not lists or matrices")
        return builder(*kids)

    def expression(self, text: str, *, expand: bool = False):
        """Parse, guard and build one expression string."""
        try:
            root = parse(text)
            check(root, expand=expand)
            if self.max_terms is not None:
                check_expansion(
                    root, max_terms=self.max_terms, max_chars=self.max_chars if expand else None
                )
        except CasInputError as exc:
            exc.source = text  # for the caret line of the error
            raise
        return self.build(root)

    def at(self, mapping) -> dict:
        """An `at` object (name -> expression) as a substitution dict for `evalf(subs=...)`."""
        if not isinstance(mapping, dict):
            raise refuse("at must be an object from names to expressions")
        result = {}
        for name, text in mapping.items():
            key = self.symbol(checked_name(name, "the at key"))
            if not isinstance(text, str):
                raise refuse(f"the value of {name} in at must be an expression string")
            result[key] = self.expression(text)
        return result


def text(value) -> str:
    """SymPy's string form of a built value."""
    try:
        return sstr(value)
    except RecursionError:
        raise MathError("the result is nested too deeply to print") from None
    except ValueError as exc:  # an integer over `sys.set_int_max_str_digits`
        raise MathError(str(exc)[:500]) from None
