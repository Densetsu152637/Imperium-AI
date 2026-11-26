"""The AST scan of plan D1: the package's own modules contain no dynamic code.

R1: no `Name` or `Attribute` whose identifier is a dynamic-code name, with listed exceptions.
R2: no `Attribute` whose attribute name starts with `_`.
R3: no import inside a function.
R4: every module imports from an allowlist, not a denylist: `parse.py`, `guard.py` and
    `build.py` import only `re`, `math`, `dataclasses` and `typing`, plus (in `build.py`)
    `from sympy import <names of the table>` and the sibling modules they are built on. A module
    with no entry below gets the default (those four, `types` and the package's own modules), and
    the modules with exceptions get exactly those. Stages 2a to 5 add their imports here.
"""

import ast
import tomllib
from pathlib import Path

import pytest

from imperium_cas.functions import FUNCTIONS

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "imperium_cas"
PYPROJECT = Path(__file__).resolve().parents[1] / "pyproject.toml"

FORBIDDEN = frozenset(
    {
        "eval",
        "exec",
        "compile",
        "__import__",
        "getattr",
        "setattr",
        "delattr",
        "globals",
        "locals",
        "vars",
        "open",
        "sympify",
        "parse_expr",
        "parse_latex",
        "lambdify",
        "S",
        "importlib",
        "pickle",
        "marshal",
        "system",
        "__builtins__",
        "builtins",
        "breakpoint",
        "attrgetter",
        "methodcaller",
        "runpy",
        "code",
        "codeop",
        # Not in R1's list, but R1 grants exceptions for them, so they are forbidden elsewhere:
        "subprocess",
        "os",
        "ctypes",
    }
)

# Attribute names that reach loaded modules (`sys.modules["os"]`), forbidden as attributes only.
FORBIDDEN_ATTRIBUTES = frozenset(
    {"modules", "get_type_hints", "make_dataclass", "ForwardRef", "resolve_name", "locate"}
)

# The only exceptions, per module (plan D1 R1).
EXCEPTIONS = {
    "pool.py": {"subprocess", "os"},
    "limits_macos.py": {"ctypes"},
    "limits_windows.py": {"ctypes"},
    "landlock.py": {"ctypes", "os", "open"},  # `os.open` of the interpreter and the roots
    "worker.py": {"os"},  # `os._exit`, `os.getpid` and `os.write` (test_worker_uses_os_narrowly)
    "limits_posix.py": {"open"},  # `/proc/self/statm` only, before the hook (test below)
}
# Attributes starting with `_` that a module may use (R2), each a documented need.
R2_EXCEPTIONS = {
    "worker.py": {"_exit"},  # the audit hook and every exit: no interpreter shutdown runs
}

BASE_IMPORTS = {"re", "math", "dataclasses", "typing"}
DEFAULT_IMPORTS = BASE_IMPORTS | {"types"}  # plus any `imperium_cas` module
# Imports beyond the default, per module (the exceptions of R1, and what each stage needs).
EXTRA_IMPORTS = {
    "pool.py": {"subprocess", "os", "signal", "sys", "threading", "time"},
    "limits_macos.py": {"ctypes", "sys"},
    "limits_windows.py": {"ctypes", "ctypes.wintypes", "sys"},
    "limits_posix.py": {"resource", "signal", "sys"},
    "landlock.py": {"ctypes", "os", "sys"},
    "framing.py": {"json"},
    "worker.py": {"os", "sys", "time"},
    "__main__.py": {"sys"},
    # Stage 3: the tools and the result printer use SymPy; the MCP front uses the SDK.
    "tools.py": {"random", "sympy"},
    "format.py": {"sympy"},
    "nocodegen.py": {"sympy.polys"},
    "server.py": {
        "itertools",
        "sys",
        "time",
        "functools",
        "anyio",
        "mcp_types",
        "jsonschema",
        "mcp.server.lowlevel",
        "mcp.server.stdio",
    },
}
# R4's list plus the sibling modules each pure module is built on. The plan's R4 names no sibling
# imports, but `guard` needs `parse`'s tree types and every module needs the function table.
R4_ALLOWED = {
    "functions.py": {"dataclasses", "types"},
    "parse.py": BASE_IMPORTS | {"imperium_cas.functions"},
    "guard.py": BASE_IMPORTS | {"imperium_cas.functions", "imperium_cas.parse"},
    "build.py": BASE_IMPORTS
    | {"types", "imperium_cas.functions", "imperium_cas.parse", "imperium_cas.guard", "sympy"},
}
# `build.py` may `from sympy import <name>` for these (stage 2a adds what its builder needs).
SYMPY_NAMES = set(FUNCTIONS) | {"Symbol", "Integer", "Rational", "Matrix", "Eq", "Ne", "Lt", "Le"}
SYMPY_NAMES |= {"Gt", "Ge", "Pow", "Add", "Mul", "I", "E", "pi", "oo"}
SYMPY_NAMES |= {"Tuple", "true", "false", "sstr"}  # stage 2a: lists, isprime's result, printing
# Stage 3: what the tools call (every one a fixed function or class, none parses a string) and
# what the printer uses. No module but these three imports SymPy.
TOOL_NAMES = {"Abs", "AccumBounds", "Complexes", "ConditionSet", "Equality", "Expr", "FiniteSet"}
TOOL_NAMES |= {"I", "Integer", "Integral", "Limit", "Matrix", "MatrixBase", "Rational", "Reals"}
TOOL_NAMES |= {"Rel", "Tuple", "Unequality", "default_sort_key", "diff", "expand", "factor"}
TOOL_NAMES |= {"factorint", "integrate", "limit", "linsolve", "nonlinsolve", "series"}
TOOL_NAMES |= {"simplify", "solveset"}
SYMPY_IMPORTERS = {
    "build.py": SYMPY_NAMES,
    "tools.py": TOOL_NAMES,
    "format.py": {"Equality", "MatrixBase", "Unequality", "latex", "sstr"},
    "nocodegen.py": {"rings"},  # `from sympy.polys import rings`, to replace its MonomialOps
}
SYMPY_SUBMODULES = {"nocodegen.py": {"sympy.polys"}}


def scan(source: str, module: str) -> list[str]:
    """Return the rule violations in `source`, for the package module named `module`."""
    tree = ast.parse(source)
    allowed_names = EXCEPTIONS.get(module, set())
    problems: list[str] = []

    def walk(node: ast.AST, in_function: bool) -> None:
        here = in_function or isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef | ast.Lambda)
        line = getattr(node, "lineno", 0)
        if isinstance(node, ast.Name) and node.id in FORBIDDEN - allowed_names:
            problems.append(f"R1 line {line}: name {node.id}")
        if isinstance(node, ast.Attribute):
            if node.attr in (FORBIDDEN | FORBIDDEN_ATTRIBUTES) - allowed_names:
                problems.append(f"R1 line {line}: attribute {node.attr}")
            if node.attr.startswith("_") and node.attr not in R2_EXCEPTIONS.get(module, set()):
                problems.append(f"R2 line {line}: attribute {node.attr}")
        if isinstance(node, ast.Import | ast.ImportFrom):
            if in_function:
                problems.append(f"R3 line {line}: import inside a function")
            problems.extend(check_import(node, module, line, allowed_names))
        for child in ast.iter_child_nodes(node):
            walk(child, here)

    walk(tree, False)
    return problems


def import_targets(node: ast.Import | ast.ImportFrom) -> tuple[list[str], list[str]]:
    """The absolute module names an import brings in, and the names imported from it."""
    if isinstance(node, ast.Import):
        return [alias.name for alias in node.names], []
    if node.level:
        base = "imperium_cas" + (f".{node.module}" if node.module else "")
        if not node.module:
            return [f"{base}.{alias.name}" for alias in node.names], []
        return [base], [alias.name for alias in node.names]
    return [node.module or ""], [alias.name for alias in node.names]


def check_import(node, module: str, line: int, allowed_names: set[str]) -> list[str]:
    problems = []
    modules, names = import_targets(node)
    for target in modules:
        if target.split(".")[0] in FORBIDDEN - allowed_names:
            problems.append(f"R1 line {line}: imports {target}")
    for name in names:
        if name in (FORBIDDEN | FORBIDDEN_ATTRIBUTES) - allowed_names:
            problems.append(f"R1 line {line}: imports the name {name}")
    allowed = R4_ALLOWED.get(module)
    if allowed is None:
        allowed = DEFAULT_IMPORTS | EXTRA_IMPORTS.get(module, set())
        if module == "__init__.py":
            allowed = set()
    for target in modules:
        if target not in allowed and not (
            module not in R4_ALLOWED and target.split(".")[0] == "imperium_cas"
        ):
            problems.append(f"R4 line {line}: {module} imports {target}")
        if target.split(".")[0] == "sympy":
            allowed_targets = {"sympy"} | SYMPY_SUBMODULES.get(module, set())
            if module not in SYMPY_IMPORTERS or not (
                isinstance(node, ast.ImportFrom) and target in allowed_targets
            ):
                problems.append(f"R4 line {line}: sympy only as `from sympy import <name>`")
            for name in names:
                if name not in SYMPY_IMPORTERS.get(module, set()):
                    problems.append(f"R4 line {line}: sympy name {name} is not in the table")
    return problems


def modules(root: Path = PACKAGE) -> list[Path]:
    return sorted(root.rglob("*.py"))


def test_subpackages_are_scanned(tmp_path):
    """`rglob`, not `glob`: a module in a subpackage is scanned too."""
    nested = tmp_path / "sub" / "deep.py"
    nested.parent.mkdir()
    nested.write_text("x = 1", encoding="utf-8")
    assert nested in modules(tmp_path)


def test_the_package_exists_and_has_its_stage_1_modules():
    names = {path.name for path in modules()}
    assert {"__init__.py", "parse.py", "guard.py", "functions.py"} <= names


def test_init_is_empty():
    """Stage 2b relies on this: importing the package must import nothing."""
    assert (PACKAGE / "__init__.py").read_text(encoding="utf-8") == ""


@pytest.mark.parametrize("path", modules(), ids=lambda path: path.name)
def test_no_dynamic_code_in_the_package(path):
    assert scan(path.read_text(encoding="utf-8"), path.name) == []


def test_the_scan_catches_what_it_should():
    cases = {
        "eval('1')": "R1",
        "exec('x = 1')": "R1",
        "compile('1', '', 'eval')": "R1",
        "re.compile('x')": "R1",
        "__import__('os')": "R1",
        "getattr(x, 'y')": "R1",
        "setattr(x, 'y', 1)": "R1",
        "delattr(x, 'y')": "R1",
        "globals()": "R1",
        "locals()": "R1",
        "vars(x)": "R1",
        "open('f')": "R1",
        "sympify('1')": "R1",
        "sympy.sympify('1')": "R1",
        "parse_expr('1')": "R1",
        "sympy.parsing.sympy_parser.parse_expr('1')": "R1",
        "parse_latex('1')": "R1",
        "lambdify(x, x)": "R1",
        "S('1')": "R1",
        "sympy.S.One": "R1",
        "importlib.import_module('os')": "R1",
        "pickle.loads(b'')": "R1",
        "marshal.loads(b'')": "R1",
        "os.system('id')": "R1",
        "subprocess.run(['id'])": "R1",
        "ctypes.CDLL(None)": "R1",
        "import pickle": "R1",
        "from os import path": "R1",
        "from sympy import sympify": "R1",
        "x.__class__": "R2",
        "x._private": "R2",
        "x.__dict__": "R2",
        "x.__init__": "R2",
        "def f():\n    import math": "R3",
        "def f():\n    from math import pi": "R3",
        "class A:\n    def f(self):\n        import re": "R3",
        "f = lambda: __import__('os')": "R1",
        "__builtins__['ev' + 'al']('1')": "R1",
        "import builtins": "R1",
        "builtins.eval('1')": "R1",
        "breakpoint()": "R1",
        "from operator import attrgetter": "R1",
        "operator.attrgetter('x')(y)": "R1",
        "operator.methodcaller('x')(y)": "R1",
        "import runpy": "R1",
        "import code": "R1",
        "import codeop": "R1",
        "code.interact()": "R1",
        "sys.modules['os']": "R1",
        "from typing import get_type_hints": "R1",
        "from typing import ForwardRef as F": "R1",
        "from dataclasses import make_dataclass": "R1",
        "from pkgutil import resolve_name": "R1",
        "from pydoc import locate": "R1",
        "from sys import modules": "R1",
        "import pkgutil": "R4",
        "pkgutil.resolve_name('os.system')": "R1",
        "import timeit": "R4",
        "import cProfile": "R4",
        "import profile": "R4",
        "import pydoc": "R4",
        "pydoc.locate('os.system')": "R1",
        "import posix": "R4",
        "import nt": "R4",
        "typing.get_type_hints(x)": "R1",
        "typing.ForwardRef('1+1')": "R1",
        "dataclasses.make_dataclass('A', [])": "R1",
        "import operator": "R4",
        "import json": "R4",
        "import functools": "R4",
        "import threading": "R4",
        "import sys": "R4",
    }
    for source, rule in cases.items():
        problems = scan(source, "other.py")
        assert any(problem.startswith(rule) for problem in problems), (source, problems)


def test_the_scan_accepts_ordinary_code():
    source = "import math\n\n\ndef f(x):\n    return math.log10(x) + len(str(x))\n"
    assert scan(source, "parse.py") == []


def test_the_exceptions_are_per_module():
    assert scan("import subprocess\nsubprocess.run([])", "pool.py") == []
    assert scan("import os\nos.getpid()", "pool.py") == []
    assert scan("import subprocess", "worker.py") != []
    assert scan("import ctypes\nctypes.CDLL(None)", "landlock.py") == []
    assert scan("import ctypes\nctypes.CDLL(None)", "limits_macos.py") == []
    assert scan("import ctypes", "pool.py") != []
    assert scan("eval('1')", "pool.py") != []  # an exception never covers `eval`


def test_r4_import_lists():
    assert scan("import json", "parse.py") != []
    assert scan("import sympy", "guard.py") != []
    assert scan("from sympy import sin, cos, Symbol", "build.py") == []
    assert scan("from sympy import Basic", "build.py") != []
    assert scan("import sympy", "build.py") != []
    assert scan("from sympy import sin", "parse.py") != []
    assert scan("from .functions import FUNCTIONS", "parse.py") == []
    assert scan("from .parse import Num", "guard.py") == []
    assert scan("from .guard import check", "parse.py") != []
    assert scan("import re, math, dataclasses, typing", "guard.py") == []
    # R4 does not apply to modules it does not name (functions.py uses `types`).
    assert scan("from types import MappingProxyType", "functions.py") == []


def test_ruff_runs_the_exec_and_eval_rules():
    config = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))["tool"]["ruff"]["lint"]
    assert "S" in config["select"]  # S102 (exec) and S307 (eval) are in the S group
    banned = {"S", "S102", "S307"}
    assert not banned & set(config.get("ignore", []))
    for rules in config.get("per-file-ignores", {}).values():
        assert not banned & set(rules)


def test_the_stage_2a_modules_exist():
    names = {path.name for path in modules()}
    assert {"build.py", "worker.py", "framing.py"} <= names


def test_worker_uses_os_narrowly():
    """The worker's `os` exception covers `os._exit` and `os.getpid`, nothing else."""
    tree = ast.parse((PACKAGE / "worker.py").read_text(encoding="utf-8"))
    used = {
        node.attr
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.value, ast.Name)
        and node.value.id == "os"
    }
    assert used == {"_exit", "getpid", "write"}  # `write`: the hook's refusal frame


def test_the_r2_exception_is_per_module():
    assert scan("import os\nos._exit(77)", "worker.py") == []
    assert scan("import os\nos._exit(77)", "build.py") != []
    assert scan("x._private", "worker.py") != []
    assert scan("import json", "framing.py") == []
    assert scan("import json", "build.py") != []
    assert scan("from sympy import sstr, Tuple, true", "build.py") == []
    assert scan("from sympy import sympify", "build.py") != []


def test_the_stage_2b_modules_exist():
    names = {path.name for path in modules()}
    assert {"pool.py", "landlock.py", "limits_posix.py", "limits_macos.py", "__main__.py"} <= names


def test_limits_posix_opens_only_statm():
    """Its `open` exception covers one fixed procfs path, read before the hook."""
    tree = ast.parse((PACKAGE / "limits_posix.py").read_text(encoding="utf-8"))
    calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "open"
    ]
    assert len(calls) == 1
    assert isinstance(calls[0].args[0], ast.Name) and calls[0].args[0].id == "STATM"
    assert 'STATM = "/proc/self/statm"' in (PACKAGE / "limits_posix.py").read_text(encoding="utf-8")


def test_the_front_never_imports_sympy_or_the_worker():
    """`pool.py`, `landlock.py` and `__main__.py` reach neither SymPy nor `worker`/`build`."""
    for name in (
        "pool.py",
        "landlock.py",
        "__main__.py",
        "limits_macos.py",
        "framing.py",
        "server.py",
    ):
        tree = ast.parse((PACKAGE / name).read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import | ast.ImportFrom):
                for target in import_targets(node)[0]:
                    assert target.split(".")[0] != "sympy", (name, target)
                    assert target not in {
                        "imperium_cas.worker",
                        "imperium_cas.build",
                        "imperium_cas.tools",
                        "imperium_cas.format",
                    }, (
                        name,
                        target,
                    )


def test_the_stage_3_modules_exist_and_sympy_stays_in_three():
    names = {path.name for path in modules()}
    assert {"server.py", "tools.py", "format.py"} <= names
    assert scan("from sympy import solveset", "tools.py") == []
    assert scan("from sympy import sympify", "tools.py") != []
    assert scan("from sympy import Basic", "format.py") != []
    assert scan("from sympy.parsing import sympy_parser", "tools.py") != []
    assert scan("from sympy import solveset", "server.py") != []
    assert scan("import anyio", "server.py") == []
    assert scan("import anyio", "tools.py") != []
