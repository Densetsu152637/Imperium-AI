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
