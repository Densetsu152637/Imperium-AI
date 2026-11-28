"""A tool's value as D6's result: plain text, LaTeX and a kind (worker side).

The text is SymPy's `sstr` with `**` written `^`, and matrices and lists written as D1's nested
lists (`[[1, 2], [3, 4]]`), so an expression, number or matrix result parses back in D1's
language (`tests/test_roundtrip.py`). Sets print in SymPy's own sorted order, and everything a
tool lists itself (eigenvalues, factors, points) is sorted by `default_sort_key` or by name, so
a result never depends on the worker's hash seed (`-I` ignores `PYTHONHASHSEED`).

Each result's text plus LaTeX is at most 64 KiB; a bigger one is a `math` error that says how big
it was. It is never cut.
"""

from sympy import Equality, MatrixBase, Unequality, latex, sstr

from .build import MathError

OUTPUT_CAP = 64 * 1024
KINDS = frozenset(
    {"expression", "set", "solutions", "matrix", "boolean", "number", "list", "unevaluated"}
)


def plain(value) -> str:
    """The text form: `sstr` with `^`, and matrices and lists as nested lists."""
    try:
        if isinstance(value, MatrixBase):
            rows = value.tolist()
            return "[" + ", ".join("[" + ", ".join(map(plain, row)) + "]" for row in rows) + "]"
        if isinstance(value, list | tuple):
            return "[" + ", ".join(map(plain, value)) + "]"
        if isinstance(value, str):
            return value
        if isinstance(value, Equality | Unequality):  # SymPy prints `Eq(a, b)` and `Ne(a, b)`
            sign = " = " if isinstance(value, Equality) else " != "
            return plain(value.lhs) + sign + plain(value.rhs)
        return "^".join(sstr(value).split("**"))  # str.split, so no SymPy call takes a string
    except RecursionError:
        raise MathError("the result is nested too deeply to print") from None
    except ValueError as exc:  # an integer over `sys.set_int_max_str_digits`
        raise MathError(str(exc)[:500]) from None


def tex(value) -> str:
    try:
        if isinstance(value, list | tuple):
            return r"\left[" + ", ".join(map(tex, value)) + r"\right]"
        if isinstance(value, str):
            return r"\text{" + value + "}"
        return latex(value)
    except RecursionError:
        raise MathError("the result is nested too deeply to print") from None
    except ValueError as exc:
        raise MathError(str(exc)[:500]) from None


def result(value, kind: str, *, text: str | None = None, latex_text: str | None = None, **extra):
    """One ok reply. `text` and `latex_text` override the printed forms (for `factor`'s integer
    factorisation, `equal`'s verdict and `limit`'s "does not exist")."""
    if kind not in KINDS:
        raise ValueError(f"unknown result kind {kind}")
    shown = plain(value) if text is None else text
    shown_tex = tex(value) if latex_text is None else latex_text
    size = len(shown.encode("utf-8")) + len(shown_tex.encode("utf-8"))
    if size > OUTPUT_CAP:
        raise MathError(
            f"the result is {size // 1024} KiB, over the 64 KiB limit; use evaluate for a"
            " decimal value, or ask for a smaller part"
        )
    return {"ok": True, "text": shown, "latex": shown_tex, "kind": kind, **extra}
