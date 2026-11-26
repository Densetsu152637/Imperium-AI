"""The function table equals the list in the plan (D1), and nothing else can be called."""

import ast
import re
from pathlib import Path

import pytest

from imperium_cas.functions import CONSTANTS, FUNCTIONS, GUARD_CLASSES
from imperium_cas.parse import ParseError, parse

PLAN = Path(__file__).resolve().parents[3] / "docs" / "plans" / "cas-mcp.md"


def plan_table() -> dict[str, set[str]]:
    """Read D1's table: guard class -> names. The plan's text is the list this test enforces."""
    lines = PLAN.read_text(encoding="utf-8").splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith("| Guard class"))
    table: dict[str, set[str]] = {}
    for line in lines[start + 2 :]:
        if not line.startswith("|"):
            break
        columns = [column.strip() for column in line.strip().strip("|").split(" | ")]
        guard_class = re.search(r"`(\w+)`", columns[0]).group(1)
        names: set[str] = set()
        for span in re.findall(r"`([^`]+)`", columns[1]):
            names.update(span.split())
        table[guard_class] = names
    return table


def test_the_plan_table_is_found():
    table = plan_table()
    assert set(table) == set(GUARD_CLASSES)
    assert sum(len(names) for names in table.values()) == 50


def test_the_table_equals_the_plan_list():
    table = plan_table()
    ours: dict[str, set[str]] = {}
    for function in FUNCTIONS.values():
        ours.setdefault(function.guard_class, set()).add(function.name)
    assert ours == table


def test_every_name_has_one_entry_and_a_known_class():
    assert len(FUNCTIONS) == 50
    for name, function in FUNCTIONS.items():
        assert function.name == name
        assert function.guard_class in GUARD_CLASSES
        assert function.min_args >= 1
        assert function.max_args is None or function.max_args >= function.min_args


def test_the_table_is_frozen():
    with pytest.raises(TypeError):
        FUNCTIONS["eval"] = FUNCTIONS["sin"]  # type: ignore[index]
    with pytest.raises(TypeError):
        del FUNCTIONS["sin"]  # type: ignore[attr-defined]
    with pytest.raises(AttributeError):
        FUNCTIONS["sin"].name = "cos"  # type: ignore[misc]


def test_capped_functions_carry_their_caps():
    caps = {
        name: (function.cap_kind, function.cap)
        for name, function in FUNCTIONS.items()
        if function.guard_class == "capped"
    }
    assert caps == {
        "prime": ("value", 10**6),
        "nextprime": ("digits", 200),
        "prevprime": ("digits", 200),
        "isprime": ("digits", 200),
        "harmonic": ("value", 10_000),
        "bernoulli": ("value", 2_000),
        "catalan": ("value", 10_000),
    }
    for function in FUNCTIONS.values():
        if function.guard_class != "capped":
            assert function.cap is None


def test_constants():
    assert CONSTANTS == {"pi", "E", "I", "oo"}
    assert not CONSTANTS & set(FUNCTIONS)


@pytest.mark.parametrize(
    "name",
    [
        "eval",
        "exec",
        "compile",
        "getattr",
        "open",
        "sympify",
        "parse_expr",
        "lambdify",
        "S",
        "Symbol",
        "Integral",
        "Eq",
        "expand",
        "solve",
        "simplify",
        "factor",
        "diff",
        "N",
    ],
)
def test_a_name_outside_the_table_cannot_be_called(name):
    assert name not in FUNCTIONS
    with pytest.raises(ParseError, match="unknown function"):
        parse(f"{name}(1)")


def test_the_table_module_does_not_import_sympy():
    """Stage 1 holds names and guard classes as data; the SymPy callables come in `build.py`."""
    import imperium_cas.functions as functions

    tree = ast.parse(Path(functions.__file__).read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            assert all(not alias.name.startswith("sympy") for alias in node.names)
        if isinstance(node, ast.ImportFrom):
            assert not (node.module or "").startswith("sympy")
