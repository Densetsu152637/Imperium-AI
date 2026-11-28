"""Plan D1 point 4: the only strings that reach SymPy are validated symbol names.

conftest.py installs the trap for the whole session: `sympify` and `parse_expr` fail on any
`str` and `lambdify` on any call, in every module that bound them. This file checks that the trap
catches SymPy's string-taking paths (`.subs`, `solve` and the like), runs the table and the
corpus through the builder under it, and checks the package's sources statically.
"""

import ast
import contextlib
from pathlib import Path

import pytest
import sympy
from sympy import Matrix, Symbol, diff, integrate, solve, solveset

from imperium_cas.build import Builder
from imperium_cas.functions import FUNCTIONS
from imperium_cas.pool import Limits, Pool
from support import VIOLATIONS, make_spawner
from test_hostile import ENTRIES

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "imperium_cas"
x = Symbol("x")

# SymPy calls that accept a string and parse it. None may get a string literal in the package.
STRING_TAKING = frozenset(
    {
        "sympify",
        "parse_expr",
        "subs",
        "xreplace",
        "replace",
        "solve",
        "solveset",
        "linsolve",
        "nonlinsolve",
        "evalf",
        "diff",
        "integrate",
        "limit",
        "series",
        "simplify",
        "expand",
        "factor",
        "Matrix",
        "Eq",
        "Ne",
        "Lt",
        "Le",
        "Gt",
        "Ge",
        "Integer",
        "Rational",
        "Float",
        "symbols",
        "Function",
        "Tuple",
    }
)


def test_the_trap_is_installed_everywhere():
    import sympy.core.basic
    import sympy.parsing.sympy_parser

    assert sympy.sympify.trapped
    assert sympy.core.basic.sympify.trapped
    assert sympy.parsing.sympy_parser.parse_expr.trapped
    assert sympy.lambdify.trapped


@pytest.mark.parametrize(
    "attempt",
    [
        lambda: sympy.sympify("1"),
        lambda: sympy.parsing.sympy_parser.parse_expr("1"),
        lambda: sympy.lambdify(x, x),
        lambda: x.subs(x, "y + 1"),
        lambda: x.subs({x: "y"}),
        lambda: solve("x - 1", x),
        lambda: solveset("x - 1", x),
        lambda: diff("x^2", x),
        lambda: integrate("x", x),
        lambda: (x + 1).evalf(subs={x: "2"}),
        lambda: Matrix([["1"]]),
    ],
    ids=[
        "sympify",
        "parse_expr",
        "lambdify",
        "subs",
        "subs-dict",
        "solve",
        "solveset",
        "diff",
        "integrate",
        "evalf-subs",
        "Matrix",
    ],
)
def test_the_trap_catches_string_taking_paths(attempt):
    before = len(VIOLATIONS)
    # SymPy may wrap the trap's error in its own; the record counts either way.
    with contextlib.suppress(Exception):
        attempt()
    assert len(VIOLATIONS) > before, "a string reached SymPy without the trap seeing it"
    del VIOLATIONS[before:]


def test_the_table_and_the_corpus_build_without_strings_or_code():
    """Every table entry and corpus line through the builder, in a limited worker with the trap
    and the real hook: no trapped call, and no `compile` or `exec` event (the hook would end the
    worker with `REFUSED`; a parse of text into code raises one of them first)."""
    inputs = [f"{name}(5)" for name in FUNCTIONS] + [text for _, text in ENTRIES]
    pool = Pool(make_spawner("trap_probe.py"), Limits(cpu_seconds=60, wall_seconds=120))
    try:
        reply = pool.call({"expressions": inputs})
    finally:
        pool.close()
    assert reply == {"ok": True, "violations": [], "trapped": []}, reply


def test_names_reach_symbol_only_after_the_identifier_check():
    builder = Builder({"x": ["real"]})
    assert builder.symbol("x").name == "x"
    tree = ast.parse((PACKAGE / "build.py").read_text(encoding="utf-8"))
    symbol_calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "Symbol"
    ]
    assert len(symbol_calls) == 1  # in `Builder.symbol`, after `checked_name`
    assert [ast.unparse(arg) for arg in symbol_calls[0].args] == ["name"]


def string_arguments(source: str) -> list[str]:
    """Calls to a string-taking SymPy name with a string literal or f-string argument."""
    found = []
    for node in ast.walk(ast.parse(source)):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        name = func.id if isinstance(func, ast.Name) else getattr(func, "attr", "")
        if name not in STRING_TAKING:
            continue
        values = [*node.args, *(keyword.value for keyword in node.keywords)]
        for value in values:
            for inner in ast.walk(value):
                if isinstance(inner, ast.JoinedStr) or (
                    isinstance(inner, ast.Constant) and isinstance(inner.value, str)
                ):
                    found.append(f"line {node.lineno}: {name}({ast.unparse(value)[:40]})")
    return found


def sympy_facing_modules() -> list[Path]:
    """Modules that import SymPy or the builder (`functions.py` has its own `Function` class)."""
    found = []
    for path in sorted(PACKAGE.rglob("*.py")):
        for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
            if isinstance(node, ast.ImportFrom) and node.module in ("sympy", "build"):
                found.append(path)
                break
    return found


def test_the_sympy_facing_modules_are_found():
    assert {path.name for path in sympy_facing_modules()} >= {"build.py", "worker.py"}


@pytest.mark.parametrize("path", sympy_facing_modules(), ids=lambda path: path.name)
def test_no_string_literal_reaches_a_string_taking_call(path):
    assert string_arguments(path.read_text(encoding="utf-8")) == []


def test_the_static_check_catches_string_arguments():
    for source in (
        'x.subs(x, "1")',
        "expr.subs({x: f'{y}'})",
        'solve("x - 1", x)',
        'Matrix([["1"]])',
        'expr.evalf(subs={x: "2"})',
        'Rational("1/2")',
    ):
        assert string_arguments(source), source
    assert string_arguments("x.subs(x, value)") == []


def test_no_tool_reaches_sympys_string_round_trips():
    """`Expr.equals` and `nsimplify` can call `mpmath.identify`, which returns a string SymPy then
    parses with `sympify` (and `eval`): none of the package's modules call them."""
    names = {"equals", "nsimplify", "identify"}
    for path in sympy_facing_modules():
        if path.name == "nocodegen.py":
            continue  # the module that turns `identify` off
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Attribute | ast.Name):
                name = node.attr if isinstance(node, ast.Attribute) else node.id
                assert name not in names, (path.name, node.lineno, name)
