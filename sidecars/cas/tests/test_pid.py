"""The PID rule (plan D3): spawn the base interpreter, not the venv's `python`.

On Windows the venv's `python.exe` is a redirector that starts the base interpreter as a second
process, so the PID the spawner holds is not the worker's. Production starts the base interpreter
under `-I` and reaches the managed environment through a `.pth` file, which `-I` still reads.
"""

import os
import subprocess
import sys
import sysconfig
import uuid
from pathlib import Path

import pytest

from imperium_cas import worker
from support import BASE_PYTHON, SRC, WORKER_TIMEOUT
from test_worker import call

BASE = BASE_PYTHON
PRINT_PID = "import os; print(os.getpid())"


def spawned_pid(executable: str) -> tuple[int, int]:
    proc = subprocess.Popen(  # noqa: S603 - the test's own interpreters
        [executable, "-I", "-c", PRINT_PID], stdout=subprocess.PIPE, text=True
    )
    out, _ = proc.communicate(timeout=WORKER_TIMEOUT)
    return proc.pid, int(out)


def test_the_base_interpreter_pid_is_the_process_pid():
    held, reported = spawned_pid(BASE)
    assert held == reported


@pytest.mark.skipif(sys.platform != "win32", reason="the venv redirector exists only on Windows")
def test_the_venv_redirector_pid_differs_on_windows():
    if os.path.normcase(sys.executable) == os.path.normcase(BASE):
        pytest.skip("not running in a venv")
    held, reported = spawned_pid(sys.executable)
    assert held != reported  # why workers are started with the base interpreter


@pytest.fixture
def base_pth(tmp_path):
    """A `.pth` in the base interpreter's site-packages naming the package and the venv's
    packages, as the managed environment's does. It is written there for the length of the test
    only, so it runs in CI, never on a developer's shared interpreter."""
    if not os.environ.get("CI"):
        pytest.skip("writes to the base interpreter's site-packages; runs in CI")
    purelib = subprocess.run(  # noqa: S603
        [BASE, "-I", "-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    pth = Path(purelib) / f"imperium_cas_test_{uuid.uuid4().hex}.pth"
    lines = [str(tmp_path), SRC, sysconfig.get_paths()["purelib"]]
    try:
        pth.write_text("\n".join(lines) + "\n", encoding="utf-8")
    except OSError as exc:
        pytest.skip(f"the base site-packages is not writable: {exc}")
    try:
        yield tmp_path
    finally:
        pth.unlink(missing_ok=True)


def test_pth_lines_are_read_under_isolated_mode(base_pth):
    (base_pth / "cas_pth_probe.py").write_text("FOUND = 1\n", encoding="utf-8")
    out = subprocess.run(  # noqa: S603
        [
            BASE,
            "-I",
            "-c",
            "import cas_pth_probe, sys; print(cas_pth_probe.FOUND, sys.flags.isolated)",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    assert out == ["1", "1"]


def test_a_worker_on_the_base_interpreter_reports_its_own_pid(base_pth):
    proc = subprocess.Popen(  # noqa: S603
        [BASE, "-I", "-B", "-m", "imperium_cas.worker"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "PYTHONHASHSEED": "0"},
    )
    ready, reply, code = call(proc, {"tool": "build", "expression": "1 + 1"})
    assert ready["pid"] == proc.pid
    assert reply == {"ok": True, "text": "2"}
    assert code == worker.EXIT_OK
