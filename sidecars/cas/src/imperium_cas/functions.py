"""The frozen function table: names, guard classes and argument counts, as data.

This is the whole list of callables the input language can name (plan D1). It holds no SymPy
import: stage 2a's `build.py` maps each name to an explicitly imported SymPy callable, and a test
checks that the two tables have the same names. A name that is not here cannot be called.

Guard classes (plan D1 and D2):

- `symbolic`: SymPy leaves these unevaluated on exact constants, or returns small exact values.
- `root`: `Pow` with a rational exponent (`sqrt(2)^(10^6)` is an eager integer power).
- `select`: computes from its inputs; the magnitude is at most the inputs' sum.
- `digits`: eager exact integers; the guard estimates the result's digit count.
- `capped`: an argument cap, because cost grows faster than the result's size.
"""

from dataclasses import dataclass
from types import MappingProxyType

SYMBOLIC = "symbolic"
ROOT = "root"
SELECT = "select"
DIGITS = "digits"
CAPPED = "capped"

GUARD_CLASSES = (SYMBOLIC, ROOT, SELECT, DIGITS, CAPPED)


@dataclass(frozen=True, slots=True)
class Function:
    """One callable of the language.

    `max_args` is `None` for a variadic function. `cap_kind` and `cap` describe a `capped`
    function's argument cap: `"value"` caps the argument's absolute value, `"digits"` caps its
    digit count.
    """

    name: str
    guard_class: str
    min_args: int
    max_args: int | None
    cap_kind: str | None = None
    cap: int | None = None


def _one(names: str, guard_class: str) -> list[Function]:
    return [Function(name, guard_class, 1, 1) for name in names.split()]


_TABLE: list[Function] = [
    *_one(
        "sin cos tan cot sec csc asin acos atan acot sinh cosh tanh coth asinh acosh atanh "
        "exp erf Abs sign re im arg conjugate ln",
        SYMBOLIC,
    ),
    Function("atan2", SYMBOLIC, 2, 2),
    Function("log", SYMBOLIC, 1, 2),
    *_one("sqrt cbrt", ROOT),
    Function("root", ROOT, 2, 2),
    *_one("floor ceiling", SELECT),
    Function("Min", SELECT, 1, None),
    Function("Max", SELECT, 1, None),
    Function("Mod", SELECT, 2, 2),
    Function("gcd", SELECT, 2, None),
    Function("lcm", SELECT, 2, None),
    *_one("factorial gamma fibonacci lucas", DIGITS),
    Function("binomial", DIGITS, 2, 2),
    Function("prime", CAPPED, 1, 1, "value", 10**6),
    Function("nextprime", CAPPED, 1, 1, "digits", 200),
    Function("prevprime", CAPPED, 1, 1, "digits", 200),
    Function("isprime", CAPPED, 1, 1, "digits", 200),
    Function("harmonic", CAPPED, 1, 1, "value", 10_000),
    Function("bernoulli", CAPPED, 1, 1, "value", 2_000),
    Function("catalan", CAPPED, 1, 1, "value", 10_000),
]

FUNCTIONS = MappingProxyType({function.name: function for function in _TABLE})

# Names that are constants, not symbols. `E` is Euler's number, `I` the imaginary unit.
CONSTANTS = frozenset({"pi", "E", "I", "oo"})
