"""Hostile inputs never run in the test process: only in limited workers (the user's rule).

The test process has no CPU or memory limit of its own beyond the run's cap (`conftest.py`), and
some corpus lines pass the guard and are stopped only by a worker's limits. So no test, and no
script under `sidecars/cas`, may hand the corpus to the worker's handlers in process: a function
that reads the corpus (`ENTRIES`, `load_corpus`, `hostile.txt` and the helpers built on them)
must not also call `handle`, `run_tool`, `tool_call`, `build_call` or a tool function. The test
workers under `tests/workers/` are exempt: they run inside a worker. Small fixed inputs may still
run in process.
"""

import ast
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS_NAMES = {"ENTRIES", "load_corpus", "corpus_inputs", "corpus_calls", "CORPUS"}
HANDLERS = {"handle", "run_tool", "tool_call", "build_call"}


def sources() -> list[Path]:
    found = []
    for path in sorted(ROOT.rglob("*.py")):
        parts = path.relative_to(ROOT).parts
        if parts[0] in {".venv", ".ruff_cache", ".hypothesis"} or parts[:2] == ("tests", "workers"):
            continue
        found.append(path)
    return found


def violations(source: str) -> list[str]:
    problems = []
    for scope in ast.walk(ast.parse(source)):
        if not isinstance(scope, ast.FunctionDef | ast.AsyncFunctionDef | ast.Module):
            continue
        body = scope.body if isinstance(scope, ast.Module) else [scope]
        nodes = [node for item in body for node in ast.walk(item)]
        if isinstance(scope, ast.Module):  # module level: outside any function
            nodes = [
                node
                for item in body
                if not isinstance(item, ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef)
                for node in ast.walk(item)
            ]
        reads_corpus = any(
            (isinstance(node, ast.Name) and node.id in CORPUS_NAMES)
            or (isinstance(node, ast.Constant) and node.value == "hostile.txt")
            for node in nodes
        )
        calls = [
            node.lineno
            for node in nodes
            if isinstance(node, ast.Call)
            and (
                (isinstance(node.func, ast.Name) and node.func.id in HANDLERS)
                or (isinstance(node.func, ast.Attribute) and node.func.attr in HANDLERS)
                or (
                    isinstance(node.func, ast.Subscript) and ast.unparse(node.func.value) == "TOOLS"
                )
            )
        ]
        if reads_corpus and calls:
            name = getattr(scope, "name", "<module>")
            problems.append(f"{name}: lines {calls}")
    return problems


def test_no_test_or_script_runs_the_corpus_in_process():
    found = {}
    for path in sources():
        problems = violations(path.read_text(encoding="utf-8"))
        if problems:
            found[str(path.relative_to(ROOT))] = problems
    assert not found, f"corpus inputs handed to the worker's handlers in process: {found}"


def test_the_check_catches_in_process_corpus_runs():
    bad = (
        "def f():\n    for _, t in ENTRIES:\n        worker.handle({'tool': 'simplify'})\n",
        "for t in load_corpus():\n    run_tool('expand', {'expression': t})\n",
        "def g():\n    return [TOOLS['factor'](b, {'expression': t}) for t in corpus_inputs()]\n",
    )
    for source in bad:
        assert violations(source), source
    good = (
        "def f():\n    worker.handle({'tool': 'simplify', 'args': {'expression': 'x'}})\n",
        "def g():\n    return pool.call({'tool': 'simplify', 'args': ENTRIES[0]})\n",
    )
    for source in good:
        assert not violations(source), source


# --- Every input the test process hands to a handler is known to be small ---

SMALL_DIGITS = 25_000  # the stage 1 estimate of a result's digits
SMALL_TERMS = 2_000  # the expansion estimate
SMALL_SIDE = 6  # rows and columns of a matrix literal


def small(text: str) -> bool:
    """A literal may run in process if the parser refuses it (pure, nothing is built) or if both
    estimates show it small. A literal the guard refuses depends on that refusal, so it belongs
    in a limited worker (`test_tools_limited.py`)."""
    from imperium_cas.guard import Est, check, expansion, size
    from imperium_cas.parse import CasInputError, ParseError, parse

    try:
        root = parse(text)
    except ParseError:
        return True
    try:
        est = check(root)
        terms = expansion(root)
    except CasInputError:
        return False
    if isinstance(est, Est):
        return size(est) <= SMALL_DIGITS and terms.worst <= SMALL_TERMS
    return max(est.rows, est.cols) <= SMALL_SIDE and terms.worst <= SMALL_TERMS


def runs_handlers(tree: ast.AST, names=HANDLERS) -> bool:
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            func = node.func
            name = func.id if isinstance(func, ast.Name) else getattr(func, "attr", "")
            if name in names:
                return True
    return False


def handler_names(tree: ast.Module) -> set[str]:
    """The handlers, plus every function of the module that calls one, directly or through
    another (a test's `ok(...)` that wraps `worker.handle`)."""
    names = set(HANDLERS)
    functions = [n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef | ast.AsyncFunctionDef)]
    while True:
        more = {f.name for f in functions if f.name not in names and runs_handlers(f, names)}
        if not more:
            return names
        names |= more


def literals(tree: ast.AST) -> list[tuple[int, str]]:
    return [
        (node.lineno, node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value
    ]


# Literals known to be refused cheaply, with why running them in process is safe even if the
# refusal regressed.
KNOWN = {
    # The warm-up's guard path (worker.py): were the guard to regress, `2^(10^7)` is about three
    # million digits, a second of CPU and a few MB, not a runaway.
    "2^(10^7)": "bounded even unrefused",
}


def scopes_with_inputs(tree: ast.Module) -> list[ast.AST]:
    """The functions that call a handler, and the module's own top level if any does (its
    constants can feed them)."""
    names = handler_names(tree)
    functions = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef) and runs_handlers(node, names)
    ]
    if not functions:
        return []
    top = [
        item
        for item in tree.body
        if not isinstance(item, ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef)
    ]
    return functions + top


def test_in_process_inputs_are_small():
    """Every string literal that can reach a handler in the test process (in a function that
    calls one, or at the top level of such a module, and in the warm-up lists, which
    `conftest.py` runs in process) parses to a small input or not at all."""
    found = {}
    modules = sorted((ROOT / "tests").glob("*.py"))
    modules += [ROOT / "src" / "imperium_cas" / name for name in ("tools.py", "worker.py")]
    for path in modules:
        if path.name == "test_no_inprocess_corpus.py":
            continue
        tree = ast.parse(path.read_text(encoding="utf-8"))
        scopes = [tree] if path.parent.name == "imperium_cas" else scopes_with_inputs(tree)
        big = sorted(
            {
                f"line {line}: {text[:40]!r}"
                for scope in scopes
                for line, text in literals(scope)
                if text not in KNOWN and not small(text)
            }
        )
        if big:
            found[path.name] = big
    assert not found, f"inputs that are not known to be small run in process: {found}"


def test_the_smallness_check():
    assert small("(x + y)^20") and small("x +") and small("not invertible")
    for text in ("(x + y + z)^5000", "(x + y)^20000", "2^(10^7)", "(x+y+z)^sqrt(25000000)"):
        assert not small(text), text


def test_the_lint_follows_wrappers_of_the_handlers():
    source = (
        "def ok(tool, **args):\n    return worker.handle({'tool': tool, 'args': args})\n\n"
        "def test_x():\n    ok('simplify', expression='(x + y + z)^5000')\n"
    )
    tree = ast.parse(source)
    texts = [text for scope in scopes_with_inputs(tree) for _, text in literals(scope)]
    assert "(x + y + z)^5000" in texts and not small("(x + y + z)^5000")
