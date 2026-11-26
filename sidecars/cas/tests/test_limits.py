"""Stage 2b: the per-call limits through the front's pool (plan D3, goal 2).

On Linux and macOS the CPU spin, the fast allocation and the blocking sleep each end with
`limit` naming their limit. On Windows the worker applies no limit itself (stage 5's broker
does), so only the wall clock, the queue and cancellation are checked here, and the spin is
shown to be stopped by the wall clock alone. Measurements go to the "cas measurements" section.
"""

import signal
import sys
import threading
import time

import pytest

from imperium_cas.framing import CPU, MEMORY, REPLY_CAP, encode_frame, read_frame
from imperium_cas.pool import Limits, Pool, PosixSpawner, Run
from support import MEASUREMENTS, make_spawner

UNIX = pytest.mark.skipif(
    sys.platform == "win32", reason="Windows limits are the broker's Job Object (stage 5)"
)
SLACK = 2.0  # seconds of scheduling noise allowed on shared CI runners


def timed(pool: Pool, request: dict, **kwargs) -> tuple[dict, float]:
    """One call through the pool, with the spare already warm, and how long it took."""
    started = time.monotonic()
    reply = pool.call(request, received=started, **kwargs)
    return reply, time.monotonic() - started


def warm(pool: Pool) -> None:
    """Start the spare if needed and wait for its ready frame, so timings exclude the warm-up."""
    if pool.spare is None:
        pool.spare = pool.spawner.spawn(pool.limits)
    ready = read_frame(pool.spare.stdout, REPLY_CAP)
    # Put the ready frame back in front of the stream, as the pool expects to read it.
    pool.spare.stdout = Prepend(ready, pool.spare.stdout)


class Prepend:
    """A stream that yields one already-read frame before the rest of `stream`."""

    def __init__(self, frame: dict, stream):
        self.head = encode_frame(frame, REPLY_CAP)
        self.stream = stream

    def read(self, size: int = -1) -> bytes:
        if self.head:
            part, self.head = self.head[:size], self.head[size:]
            return part
        return self.stream.read(size)

    def close(self) -> None:
        self.stream.close()


@pytest.fixture
def pools():
    """Pools made by a test are closed (spare killed) when it ends."""
    made: list[Pool] = []

    def make(script=None, *extra, **limits) -> Pool:
        pool = Pool(make_spawner(script, *extra), Limits(**limits))
        made.append(pool)
        return pool

    yield make
    for pool in made:
        pool.close()


@UNIX
def test_a_cpu_spin_ends_with_the_cpu_limit(pools):
    pool = pools("spin.py", cpu_seconds=2, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"spin": True})
    assert reply["category"] == "limit" and reply["limit"] == "cpu", reply
    assert reply["message"] == "limit: CPU time (2 s)"
    # Soft limit = ceil(CPU used after warm-up) + 2 s, so at most 3 s of CPU after the call.
    assert elapsed <= 2 + 1 + SLACK, elapsed
    MEASUREMENTS.append(f"cpu spin, limit 2 s ({sys.platform}): stopped after {elapsed:.2f} s")


@UNIX
def test_the_hard_cpu_limit_stops_a_worker_that_never_runs_the_handler(pools):
    pool = pools("spin.py", "--ignore-xcpu", cpu_seconds=2, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"spin": True})
    MEASUREMENTS.append(
        f"cpu spin ignoring SIGXCPU, limit 2 s ({sys.platform}): {reply['message']}"
        f" after {elapsed:.2f} s"
    )
    assert reply["category"] == "limit" and reply["limit"] == "cpu", reply
    assert elapsed <= 2 + 2 + SLACK, elapsed


@UNIX
def test_a_fast_allocation_ends_with_the_memory_limit(pools):
    pool = pools("alloc.py", memory_mib=256, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"alloc": True})
    assert reply["category"] == "limit" and reply["limit"] == "memory", reply
    assert reply["message"] == "limit: memory (256 MiB)"
    assert elapsed <= 1 + SLACK, elapsed
    if sys.platform == "darwin":
        # The footprint poll, not a hard cap: record how far over the limit the worker got.
        over = (reply["footprint"] - reply["footprint_limit"]) / 2**20
        MEASUREMENTS.append(
            f"macOS memory poll, limit 256 MiB: killed {elapsed:.2f} s after the call,"
            f" {over:.0f} MiB over the limit at the poll that killed it"
        )
        assert over < 2048, over  # the bound 05 states
    else:
        MEASUREMENTS.append(f"memory, limit 256 MiB (linux): MemoryError after {elapsed:.2f} s")


def test_a_blocking_sleep_ends_with_the_wall_clock(pools):
    pool = pools("sleep.py", wall_seconds=2)
    warm(pool)
    reply, elapsed = timed(pool, {"sleep": True})
    assert reply == {
        "ok": False,
        "category": "limit",
        "message": "limit: wall clock (2 s)",
        "limit": "wall clock",
    }
    assert 2 <= elapsed <= 2 + 1 + SLACK, elapsed
    MEASUREMENTS.append(f"sleep, wall clock 2 s ({sys.platform}): stopped after {elapsed:.2f} s")


@pytest.mark.skipif(sys.platform != "win32", reason="shows the Windows gap until stage 5")
def test_on_windows_a_spin_is_stopped_only_by_the_wall_clock(pools):
    pool = pools("spin.py", cpu_seconds=1, wall_seconds=3)
    warm(pool)
    reply, elapsed = timed(pool, {"spin": True})
    assert reply["limit"] == "wall clock", reply
    MEASUREMENTS.append(
        f"cpu spin, limit 1 s, wall clock 3 s (win32): {reply['message']} after {elapsed:.2f} s"
        " (the worker applies no CPU limit on Windows; the broker will)"
    )


def test_posix_spawner_refuses_windows_without_the_opt_in():
    if sys.platform == "win32":
        with pytest.raises(RuntimeError, match="broker"):
            PosixSpawner()
    else:
        assert PosixSpawner().command[1:] == ["-I", "-B", "-m", "imperium_cas.worker"]


GOAL_2 = (
    "10^10^10",
    "1e999999999",
    "sqrt(2)^(10^6)",
    "factorial(factorial(20))",
    "factorial(10^7)",
    "expand((x+y+z)^5000)",
    "integrate(exp(x^x^x), x)",
    "[[2,1],[1,1]]^(10^6)",
    "(" * 50_000 + "1" + ")" * 50_000,
    "det([" + ",".join("[" + ",".join(f"x+{i}*{j}" for j in range(30)) + "]" for i in range(30)),
)


def test_goal_2_inputs_are_refused_or_stopped_and_the_next_call_works(pools):
    """Each goal 2 input gives `guard`, `parse` or `limit` within the wall clock plus 1 s.

    Stage 2b has only the `build` call, so `expand`, `integrate` and `det` are unknown names
    (`parse`); stage 3 runs them again through its tools."""
    pool = pools(cpu_seconds=5, wall_seconds=10)
    for expression in GOAL_2:
        reply, elapsed = timed(pool, {"tool": "build", "expression": expression})
        assert reply["category"] in {"guard", "parse", "limit"}, (expression[:40], reply)
        assert elapsed <= 10 + 1 + SLACK, (expression[:40], elapsed)
    reply, _ = timed(pool, {"tool": "build", "expression": "x^2 + 2x"})
    assert reply == {"ok": True, "text": "x**2 + 2*x"}


@UNIX
def test_a_normal_call_succeeds_right_after_each_limit(pools):
    for script, limits in (
        ("spin.py", {"cpu_seconds": 1}),
        ("alloc.py", {"memory_mib": 128}),
        ("sleep.py", {"wall_seconds": 1}),
    ):
        pool = pools(script, **limits)
        reply, _ = timed(pool, {"go": True})
        assert reply["category"] == "limit", (script, reply)
    pool = pools(cpu_seconds=1, memory_mib=128, wall_seconds=5)
    reply, _ = timed(pool, {"tool": "build", "expression": "factorial(20)"})
    assert reply == {"ok": True, "text": "2432902008176640000"}


def test_time_spent_queued_counts_against_the_deadline(pools):
    pool = pools("sleep.py", wall_seconds=2)
    warm(pool)
    received = time.monotonic()
    results: dict[str, dict] = {}

    def submit(name: str) -> None:
        results[name] = pool.call({"sleep": True}, call_id=name, received=received)

    first = threading.Thread(target=submit, args=("first",))
    first.start()
    while pool.running is None:
        time.sleep(0.01)
    second = threading.Thread(target=submit, args=("second",))
    second.start()
    first.join(30)
    second.join(30)
    assert results["first"]["message"] == "limit: wall clock (2 s)"
    assert results["second"]["message"] == "limit: wall clock (queued, 2 s)"
    assert time.monotonic() - received <= 2 + 1 + SLACK


def test_a_cancel_kills_the_running_worker(pools):
    pool = pools("sleep.py", wall_seconds=60)
    warm(pool)
    result: dict = {}
    thread = threading.Thread(target=lambda: result.update(pool.call({}, call_id=7)))
    thread.start()
    while pool.running is None:
        time.sleep(0.01)
    proc = pool.running.proc
    started = time.monotonic()
    pool.cancel(7)
    thread.join(30)
    assert result == {
        "ok": False,
        "category": "cancelled",
        "message": "cancelled: the call was cancelled",
    }
    assert proc.poll() is not None  # the worker is gone
    assert time.monotonic() - started <= 1 + SLACK


def test_a_cancel_of_a_queued_call_drops_it_before_it_reaches_a_worker(pools):
    pool = pools("sleep.py", wall_seconds=3)
    warm(pool)
    results: dict = {}

    def submit(name: str) -> None:
        results[name] = pool.call({}, call_id=name)

    first = threading.Thread(target=submit, args=("first",))
    first.start()
    while pool.running is None:
        time.sleep(0.01)
    second = threading.Thread(target=submit, args=("second",))
    second.start()
    while "second" not in pool.queued:
        time.sleep(0.01)
    pool.cancel("second")
    first.join(30)
    second.join(30)
    assert results["first"]["limit"] == "wall clock"
    assert results["second"]["category"] == "cancelled"


def test_exit_codes_map_to_categories():
    pool = Pool(make_spawner(), Limits())

    def outcome(code: int) -> dict:
        return pool.outcome(Run(None, None, 0.0), None, code)

    assert outcome(MEMORY)["limit"] == "memory"
    assert outcome(CPU)["limit"] == "cpu"
    assert outcome(77)["category"] == "refused"
    assert outcome(70)["category"] == "crashed"
    assert outcome(65)["category"] == "crashed"
    assert outcome(0)["category"] == "crashed"  # exit 0 with no reply
    if sys.platform != "win32":
        assert outcome(-signal.SIGXCPU)["limit"] == "cpu"
        assert outcome(-signal.SIGKILL)["limit"] == "cpu"  # the hard limit, not the front
        assert outcome(-signal.SIGSEGV)["category"] == "crashed"


def test_limits_are_range_checked():
    for bad in ({"cpu_seconds": 0}, {"cpu_seconds": 121}, {"memory_mib": 63}):
        with pytest.raises(ValueError):
            Limits(**bad)
    with pytest.raises(ValueError):
        Limits(wall_seconds=301)
    assert Limits().argv() == ["--cpu-seconds", "15", "--memory-mib", "1024"]


def test_overhead_of_a_call_through_the_pool(pools):
    """Lazy start (the first call pays the warm-up), then a warm spare per call."""
    pool = pools()
    started = time.monotonic()
    first = pool.call({"tool": "build", "expression": "x + 1"})
    cold = time.monotonic() - started
    assert first == {"ok": True, "text": "x + 1"}
    timings = []
    for _ in range(3):
        warm(pool)
        reply, elapsed = timed(pool, {"tool": "build", "expression": "(x+1)^2"})
        assert reply == {"ok": True, "text": "(x + 1)**2"}
        timings.append(elapsed)
    MEASUREMENTS.append(
        f"pool ({sys.platform}): first call {cold:.2f} s (lazy start), warm calls"
        f" {min(timings) * 1000:.0f} to {max(timings) * 1000:.0f} ms"
    )
