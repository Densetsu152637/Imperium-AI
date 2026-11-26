"""The worker: one call in, one reply out, then exit; frames are capped before allocation."""

import io
import sys
import threading
import time

import pytest

from imperium_cas import worker
from imperium_cas.framing import (
    REPLY_CAP,
    REQUEST_CAP,
    FrameError,
    encode_frame,
    read_frame,
)
from support import MEASUREMENTS, WORKER_TIMEOUT, spawn_worker


def call(proc, request: dict | None = None, raw: bytes | None = None):
    """Wait for the ready frame, send one request, and return `(ready, reply, exit code)`.

    `reply` is `None` when the worker wrote nothing after its ready frame. A watchdog kills a
    worker that takes longer than `WORKER_TIMEOUT`, so a hang fails instead of blocking.
    """
    watchdog = threading.Timer(WORKER_TIMEOUT, proc.kill)
    watchdog.start()
    try:
        try:
            ready = read_frame(proc.stdout, REPLY_CAP)
        except FrameError:
            proc.wait()
            pytest.fail(f"no ready frame; stderr: {proc.stderr.read().decode(errors='replace')}")
        try:  # a worker that exits before reading (no hook, say) closes the pipe first
            proc.stdin.write(raw if raw is not None else encode_frame(request, REQUEST_CAP))
            proc.stdin.close()
        except OSError:
            pass
        try:
            reply = read_frame(proc.stdout, REPLY_CAP)
        except FrameError:
            reply = None
        code = proc.wait()
        proc.stdout.close()
        proc.stderr.close()
    finally:
        watchdog.cancel()
    return ready, reply, code


def process_memory(proc) -> int | None:
    """The worker's peak memory, read from outside: the worker may not open files or load
    `ctypes`, and on Linux its own `ru_maxrss` carries over the parent's peak across `exec`.
    Linux: `VmHWM`; macOS: the current resident size from `ps`; Windows: the peak working set."""
    if sys.platform.startswith("linux"):
        with open(f"/proc/{proc.pid}/status", encoding="ascii") as status:
            for line in status:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
        return None
    if sys.platform == "darwin":
        import subprocess

        out = subprocess.run(  # noqa: S603 - a fixed system tool
            ["/bin/ps", "-o", "rss=", "-p", str(proc.pid)],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip()
        return int(out) * 1024 if out.isdigit() else None
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    psapi = ctypes.WinDLL("psapi")
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD]
    handle = wintypes.HANDLE(int(proc._handle))
    if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
        return None
    return counters.PeakWorkingSetSize


def test_one_call_in_one_reply_out_then_exit():
    started = time.perf_counter()
    proc = spawn_worker("--cpu-seconds", "15", "--memory-mib", "1024")
    ready = read_frame(proc.stdout, REPLY_CAP)
    ready_after = time.perf_counter() - started
    memory = process_memory(proc)
    proc.stdin.write(encode_frame({"tool": "build", "expression": "x^2 + 2x"}, REQUEST_CAP))
    proc.stdin.close()
    reply = read_frame(proc.stdout, REPLY_CAP)
    assert proc.stdout.read() == b""  # nothing after the one reply
    assert proc.wait(WORKER_TIMEOUT) == worker.EXIT_OK
    proc.stdout.close()
    proc.stderr.close()
    assert ready["pid"] == proc.pid  # spawned with the base interpreter (plan D3)
    assert ready["ready"] and ready["limits"] == {"--cpu-seconds": 15, "--memory-mib": 1024}
    assert ready["audit_hook"] is True  # installed before the request is read
    assert reply == {"ok": True, "text": "x**2 + 2*x"}
    applied = ready["applied"]
    if sys.platform == "win32":
        assert applied == {"cpu": "requested (broker)", "memory": "requested (broker)"}
    else:
        assert applied["cpu"] == "rlimit" and applied["core"] == 0
        assert applied["cpu_hard"] == applied["cpu_soft"] + 1
        assert applied["cpu_soft"] >= 15
        if sys.platform == "darwin":
            assert applied["memory"] == "front poll" and applied["address_space"] is None
        else:
            assert applied["memory"] == "rlimit"
            assert applied["address_space"] > 1024 * 2**20
    shown = f"{memory / 2**20:.0f} MiB" if memory else "unknown"
    space = applied.get("address_space")
    MEASUREMENTS.append(
        f"worker ({sys.platform}): warm-up {ready['warmup_seconds']} s, limits applied in"
        f" {ready['limits_seconds'] * 1000:.2f} ms, ready {ready_after:.2f} s after spawn, peak"
        f" memory {shown}, applied {applied['cpu']} CPU / {applied['memory']} memory"
        + (f", RLIMIT_AS {space / 2**20:.0f} MiB" if space else "")
    )


def test_a_number_over_the_default_digit_limit_prints():
    """`10^5000` has 5,001 digits, over CPython's default of 4,300: the worker raises the cap."""
    _, reply, code = call(spawn_worker(), {"tool": "build", "expression": "10^5000"})
    assert reply == {"ok": True, "text": "1" + "0" * 5000}
    assert code == worker.EXIT_OK


@pytest.mark.parametrize("tool", [["build"], {"a": 1}, 7, None])
def test_a_tool_that_is_not_a_string_is_unknown(tool):
    reply = worker.handle({"tool": tool, "expression": "1"})
    assert reply == {"ok": False, "category": "parse", "message": "parse: unknown tool"}


def test_the_request_is_not_read_without_the_hook():
    """A worker whose hook was never installed says so, and exits before reading the call."""
    proc = spawn_worker(script="hookless_probe.py")
    ready, reply, code = call(proc, {"tool": "build", "expression": "1"})
    assert ready["audit_hook"] is False
    assert reply is None
    assert code == worker.CRASHED


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="reads /proc/<pid>/limits")
def test_core_files_are_off_from_outside():
    proc = spawn_worker()
    ready = read_frame(proc.stdout, REPLY_CAP)
    with open(f"/proc/{proc.pid}/limits", encoding="ascii") as limits:
        lines = {line[:26].strip(): line[26:].split() for line in limits}
    proc.kill()
    proc.wait()
    proc.stdout.close()
    proc.stderr.close()
    assert lines["Max core file size"][:2] == ["0", "0"]
    applied = ready["applied"]
    assert lines["Max cpu time"][:2] == [str(applied["cpu_soft"]), str(applied["cpu_hard"])]
    assert lines["Max address space"][:2] == [str(applied["address_space"])] * 2


def test_refusals_come_back_as_replies():
    _, reply, code = call(spawn_worker(), {"tool": "build", "expression": "2^(10^7)"})
    assert reply["ok"] is False and reply["category"] == "guard"
    assert code == worker.EXIT_OK
    _, reply, _ = call(spawn_worker(), {"tool": "eval", "expression": "1"})
    assert reply == {"ok": False, "category": "parse", "message": "parse: unknown tool"}


def test_an_oversized_length_prefix_is_refused_before_any_allocation():
    """A 4 GiB prefix and no body: the worker exits at once instead of waiting to read it."""
    _, reply, code = call(spawn_worker(), raw=b"\xff\xff\xff\xff")
    assert reply is None
    assert code == worker.FRAME
    _, reply, code = call(spawn_worker(), raw=(REQUEST_CAP + 1).to_bytes(4, "big"))
    assert code == worker.FRAME


def test_a_bad_command_line_exits_before_warm_up():
    proc = spawn_worker("--cpu-seconds", "9999")
    assert proc.wait(WORKER_TIMEOUT) == worker.USAGE
    proc.stdout.close()
    proc.stderr.close()


@pytest.mark.parametrize(
    "argv",
    [
        ["--cpu-seconds"],
        ["--cpu-seconds", "0"],
        ["--cpu-seconds", "121"],
        ["--cpu-seconds", "-1"],
        ["--cpu-seconds", "1e3"],
        ["--cpu-seconds", "１"],
        ["--cpu-seconds", "5", "--cpu-seconds", "6"],
        ["--wall-seconds", "5"],
        ["--memory-mib", "9000"],
    ],
)
def test_parse_limits_refuses(argv):
    with pytest.raises(ValueError):
        worker.parse_limits(argv)


def test_parse_limits_accepts():
    assert worker.parse_limits([]) == {"--cpu-seconds": 15, "--memory-mib": 1024}
    assert worker.parse_limits(["--memory-mib", "64"]) == {
        "--memory-mib": 64,
        "--cpu-seconds": 15,
    }
    assert worker.parse_limits(["--memory-mib", "64", "--cpu-seconds", "120"]) == {
        "--memory-mib": 64,
        "--cpu-seconds": 120,
    }


class Recorder(io.BytesIO):
    """A stream that records the size of every read."""

    def __init__(self, data: bytes):
        super().__init__(data)
        self.reads: list[int] = []

    def read(self, size=-1):
        self.reads.append(size)
        return super().read(size)


def test_the_reader_checks_the_prefix_before_reading_the_body():
    stream = Recorder(b"\xff\xff\xff\xff" + b"x" * 100)
    with pytest.raises(FrameError, match="over the cap"):
        read_frame(stream, REQUEST_CAP)
    assert stream.reads == [4]


@pytest.mark.parametrize(
    "data",
    [
        b"",
        b"\x00\x00",
        b"\x00\x00\x00\x05{}",
        b"\x00\x00\x00\x02[]",
        b"\x00\x00\x00\x03\xff\xfe\xfd",
        b"\x00\x00\x00\x01{",
    ],
)
def test_bad_frames_are_refused(data):
    with pytest.raises(FrameError):
        read_frame(io.BytesIO(data), REQUEST_CAP)


def test_frames_round_trip_and_replies_are_capped():
    value = {"ok": True, "text": "x**2 + 2*x", "n": [1, 2]}
    assert read_frame(io.BytesIO(encode_frame(value, REPLY_CAP)), REPLY_CAP) == value
    with pytest.raises(FrameError):
        encode_frame({"text": "x" * REPLY_CAP}, REPLY_CAP)
    nested = b"[" * 100_000 + b"]" * 100_000
    with pytest.raises(FrameError):
        read_frame(io.BytesIO(len(nested).to_bytes(4, "big") + nested), REQUEST_CAP)
