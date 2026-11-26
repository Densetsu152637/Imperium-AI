"""Stage 2b: the front's Landlock execute rule (plan D4, R2-3).

The ruleset comes first: an AST check of `__main__.py` and `landlock.py`, and at run time the
stdin reader refuses to read before `confine()` has recorded its result. On Linux with Landlock,
`execve("/bin/sh")` is refused with `EACCES` for the confined process and for a child it starts,
while the interpreter can still start itself and serve a call through a worker. Each check runs
in a subprocess, so the test session itself is never confined.
"""

import ast
import errno
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from imperium_cas import landlock
from imperium_cas.framing import REQUEST_CAP, encode_frame
from support import BASE_PYTHON, WORKER_TIMEOUT, worker_env

PACKAGE = Path(__file__).resolve().parents[1] / "src" / "imperium_cas"
LINUX = sys.platform.startswith("linux")


def landlock_abi() -> int | None:
    if not LINUX:
        return None
    try:
        return landlock.abi()
    except OSError:
        return None


ABI = landlock_abi()
NEEDS_LANDLOCK = pytest.mark.skipif(
    ABI is None, reason="Landlock is absent here (not Linux, a kernel before 5.13, or disabled)"
)


def run_python(code: str, stdin: bytes = b"") -> subprocess.CompletedProcess:
    return subprocess.run(  # noqa: S603 - the test interpreter and fixed code
        [BASE_PYTHON, "-B", "-c", code],
        input=stdin,
        capture_output=True,
        env=worker_env(),
        timeout=WORKER_TIMEOUT,
        check=False,
    )


def test_the_ruleset_is_the_first_statement_of_main():
    tree = ast.parse((PACKAGE / "__main__.py").read_text(encoding="utf-8"))
    body = tree.body
    if isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant):
        body = body[1:]  # the docstring
    first, second = body[0], body[1]
    assert isinstance(first, ast.ImportFrom)
    assert first.module == "imperium_cas.landlock" and [a.name for a in first.names] == ["confine"]
    assert isinstance(second, ast.Assign) and isinstance(second.value, ast.Call)
    assert isinstance(second.value.func, ast.Name) and second.value.func.id == "confine"


def test_landlock_imports_only_ctypes_os_and_sys():
    tree = ast.parse((PACKAGE / "landlock.py").read_text(encoding="utf-8"))
    imported = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            imported |= {alias.name for alias in node.names}
        elif isinstance(node, ast.ImportFrom):
            imported.add(node.module or "")
    assert imported == {"ctypes", "os", "sys"}


def test_the_stdin_reader_checks_the_record_before_its_first_read():
    """`serve` calls `require_confined()` before anything else, and that exits when
    `confine()` never ran (as in this test session)."""
    tree = ast.parse((PACKAGE / "__main__.py").read_text(encoding="utf-8"))
    serve = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "serve")
    first = serve.body[1] if isinstance(serve.body[0].value, ast.Constant) else serve.body[0]
    assert isinstance(first.value, ast.Call) and first.value.func.id == "require_confined"
    assert landlock.STATE["confinement"] is None
    with pytest.raises(SystemExit) as exited:
        landlock.require_confined()
    assert exited.value.code == landlock.FAILED


def test_the_front_starts_and_ends_with_stdin():
    """`python -m imperium_cas` confines itself, then serves until stdin ends."""
    result = run_python("import runpy; runpy.run_module('imperium_cas', run_name='__main__')")
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert result.stdout == b""


@pytest.mark.skipif(not LINUX, reason="Landlock is Linux only")
def test_confinement_is_reported():
    result = run_python("from imperium_cas import landlock; print(landlock.confine())")
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    expected = "no-landlock" if ABI is None else "landlock"
    assert result.stdout.decode().strip() == expected


@pytest.mark.skipif(LINUX, reason="checks the value off Linux")
def test_confinement_off_linux():
    assert (
        run_python("from imperium_cas import landlock; print(landlock.confine())")
        .stdout.decode()
        .strip()
        == "not-linux"
    )


EXEC_PROBE = """
import json, os, subprocess, sys
from imperium_cas import landlock
print(landlock.confine())
child = subprocess.run(
    [sys.executable, "-c", "import os\\ntry:\\n    os.execv('/bin/sh', ['sh', '-c', 'exit 0'])\\n"
     "except OSError as exc:\\n    print(exc.errno)"],
    capture_output=True, text=True,
)
print(child.stdout.strip() or child.returncode)
from imperium_cas.pool import Limits, Pool
from support import make_spawner
pool = Pool(make_spawner(), Limits(wall_seconds=60))
reply = pool.call({"tool": "build", "expression": "x + 1"})
pool.close()
if reply.get("text") != "x + 1":
    import subprocess as sp
    from imperium_cas.framing import REQUEST_CAP, encode_frame
    from support import worker_env
    probe = sp.run(
        make_spawner().command + Limits().argv(),
        input=encode_frame({"tool": "build", "expression": "x + 1"}, REQUEST_CAP),
        capture_output=True, timeout=60, env=worker_env(),
    )
    reply["direct"] = [probe.returncode, probe.stdout[-300:].decode(errors="replace")]
    reply["stderr"] = probe.stderr.decode(errors="replace")[-3000:]
print(json.dumps(reply))
try:
    os.execv("/bin/sh", ["sh", "-c", "exit 0"])
except OSError as exc:
    print(exc.errno)
"""


@NEEDS_LANDLOCK
def test_execve_is_refused_for_the_front_and_its_children():
    sh = os.path.realpath("/bin/sh")
    if sh.startswith(os.path.realpath(sys.base_prefix) + os.sep):
        pytest.skip("the interpreter is installed where /bin/sh is; the rule allows both")
    result = run_python(EXEC_PROBE)
    lines = result.stdout.decode().splitlines()
    assert len(lines) == 4, (lines, result.stderr.decode(errors="replace"))
    assert lines[:2] == ["landlock", str(errno.EACCES)]
    assert json.loads(lines[2]) == {"ok": True, "text": "x + 1"}  # a worker starts and answers
    assert lines[3] == str(errno.EACCES)


INJECTED = """
import runpy
from imperium_cas import landlock

def fail(ruleset):
    raise OSError(1, "injected failure of landlock_restrict_self")

landlock.restrict_self = fail
runpy.run_module("imperium_cas", run_name="__main__")
"""


@NEEDS_LANDLOCK
def test_a_failed_ruleset_exits_before_stdin_is_read():
    request = encode_frame({"tool": "build", "expression": "1"}, REQUEST_CAP)
    result = run_python(INJECTED, stdin=request)
    assert result.returncode == landlock.FAILED
    assert result.stdout == b""  # nothing was served
    assert b"injected failure" in result.stderr


def test_the_elf_loader_is_found():
    if not LINUX:
        pytest.skip("ELF is Linux here")
    loader = landlock.elf_interpreter(os.path.realpath(sys.executable))
    assert loader is None or os.path.exists(loader)
    roots = landlock.exec_roots()
    assert roots[0] == os.path.realpath(sys.base_prefix)
