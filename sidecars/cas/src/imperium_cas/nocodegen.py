"""SymPy without run-time code generation or string parsing (plan D1, the audit hook).

SymPy's polynomial rings build their monomial functions with `exec` of generated source, once
per ring (`sympy.polys.monomials.MonomialOps`). A ring is made for every new set of generators
(`nonlinsolve`'s Groebner bases, for one), so a call under the worker's audit hook would raise a
`compile` event and be refused. This module gives `sympy.polys.rings` plain Python functions of
the same meaning instead, so no code is generated at run time. The worker installs it before
its warm-up; `tests/test_audit_hook.py` checks that a fresh ring raises no event.

SymPy's `nsimplify` asks `mpmath.identify` for a closed form of a decimal: `identify` `eval`s
constant names and returns a string, which `nsimplify` parses with `sympify` (`parse_expr`, then
`eval`). SymPy reaches it from inside other functions too (`Expr.equals`, which relational and
equation simplification call), on paths that depend on the hash seed, so a call could be refused
with `compile` now and then. `install` makes `identify` find nothing, and `nsimplify` then falls
back to a plain rational approximation, as it does whenever `identify` fails.
"""

import mpmath
from sympy.polys import rings


class MonomialOps:
    """The monomial operations on exponent tuples, as `sympy.polys.monomials.MonomialOps` has
    them, without generated code. Every tuple has `ngens` entries."""

    def __init__(self, ngens: int):
        self.ngens = ngens

    def mul(self):
        return lambda a, b: tuple(x + y for x, y in zip(a, b, strict=True))

    def pow(self):
        return lambda a, k: tuple(x * k for x in a)

    def mulpow(self):
        return lambda a, b, k: tuple(x + y * k for x, y in zip(a, b, strict=True))

    def ldiv(self):
        return lambda a, b: tuple(x - y for x, y in zip(a, b, strict=True))

    def div(self):
        def div(a, b):
            result = tuple(x - y for x, y in zip(a, b, strict=True))
            return None if any(r < 0 for r in result) else result

        return div

    def lcm(self):
        return lambda a, b: tuple(x if x >= y else y for x, y in zip(a, b, strict=True))

    def gcd(self):
        return lambda a, b: tuple(x if x <= y else y for x, y in zip(a, b, strict=True))


def no_closed_form(*args, **kwargs) -> None:
    """`mpmath.identify` that never finds one (see above)."""
    return None


def install() -> None:
    """Make every polynomial ring created from now on use `MonomialOps` above, and stop
    `nsimplify` from parsing `mpmath.identify`'s strings."""
    rings.MonomialOps = MonomialOps
    mpmath.identify = no_closed_form
